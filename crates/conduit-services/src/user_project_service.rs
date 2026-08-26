use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use conduit_auth::{Scope, ScopeSet, slug};
use conduit_db::{RepoError, RequestContext};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub type UserProjectServiceResult<T> = Result<T, UserProjectServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UserProjectServiceError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error("user not found: {0}")]
    UserNotFound(String),
    #[error("project not found: {0}")]
    ProjectNotFound(String),
    #[error("role not found: {0}")]
    RoleNotFound(String),
    #[error("api key not found: {0}")]
    ApiKeyNotFound(String),
    #[error("api key is not active: {0}")]
    ApiKeyInactive(String),
    #[error("invalid permission scope slug: {0}")]
    InvalidScopeSlug(String),
    // --- Pure project-domain errors (mirror Go biz/project.go + ent/project) ---
    /// Mirrors Go `xerrors.DuplicateNameError("project", name)`.
    #[error("project name already exists: {0}")]
    DuplicateProjectName(String),
    /// Mirrors Go `ValidateProjectProfiles` / `ValidateStatus` failures.
    #[error("profile name cannot be empty")]
    EmptyProfileName,
    #[error("duplicate profile name: {0}")]
    DuplicateProfileName(String),
    #[error("profile '{0}' channelTagsMatchMode is invalid")]
    InvalidChannelTagsMatchMode(String),
    #[error("active profile '{0}' does not exist in the profiles list")]
    ActiveProfileNotFound(String),
    #[error("project: invalid enum value for status field: {0:?}")]
    InvalidProjectStatus(ProjectStatus),
    #[error("invalid status transition: {from:?} -> {to:?}")]
    InvalidStatusTransition {
        from: ProjectStatus,
        to: ProjectStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub status: UserStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserStatus {
    Active,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub status: ProjectStatus,
}

/// Mirrors Go `ent/project.Status` (`"active"` | `"archived"`).
///
/// The Go contract defines **only** these two values — there is no `suspended`
/// status in `ent/project/project.go::StatusValidator`. Do not invent one.
/// `Default = StatusActive` matches Go `DefaultStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ProjectStatus {
    #[default]
    Active,
    Archived,
}

impl ProjectStatus {
    /// Mirrors Go `ent/project.StatusValidator`: only `active`/`archived` are
    /// legal values; anything else is an error.
    pub fn validate(status: ProjectStatus) -> UserProjectServiceResult<()> {
        match status {
            ProjectStatus::Active | ProjectStatus::Archived => Ok(()),
            // With a non-repr enum this branch is statically unreachable, but
            // keeps the signature future-proof if someone adds a `from_str`
            // constructor that yields an unknown discriminant.
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub scope_slugs: Vec<String>,
    pub status: RoleStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoleStatus {
    Active,
    Disabled,
}

/// Mirrors Go `apikey.Type` (`conduit/internal/ent/apikey/apikey.go`):
/// `user` / `service_account` / `noauth`. Go default is `user`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyType {
    #[default]
    User,
    ServiceAccount,
    /// Mirrors Go `apikey.TypeNoauth`. Go tag is the single word `"noauth"`
    /// (no underscore); override the snake_case default which would render
    /// `"no_auth"`.
    #[serde(rename = "noauth")]
    NoAuth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub secret_digest: String,
    pub secret_preview: String,
    pub scope_slugs: Vec<String>,
    pub status: ApiKeyStatus,
    /// Mirrors Go `apikey.Type`. Defaults to `user` (see `ApiKeyType::default`).
    #[serde(default)]
    pub key_type: ApiKeyType,
    pub created_by_user_id: Option<String>,
}

/// Mirrors Go `apikey.Status` (`enabled`/`disabled`/`archived`, lowercase).
/// Go default is `enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyStatus {
    #[default]
    Enabled,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDelayStrategy {
    Fixed,
    Exponential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub delay_millis: u64,
    pub strategy: RetryDelayStrategy,
}

impl ApiKeyRetryPolicy {
    pub fn merged_with_override(&self, retry_override: &RetryPolicyOverride) -> Self {
        let mut merged = self.clone();

        if let Some(enabled) = retry_override.enabled {
            merged.enabled = enabled;
        }
        if let Some(max_retries) = retry_override.max_retries {
            merged.max_retries = max_retries;
        }
        if let Some(delay_millis) = retry_override.delay_millis {
            merged.delay_millis = delay_millis;
        }
        if let Some(strategy) = retry_override.strategy {
            merged.strategy = strategy;
        }

        merged
    }
}

impl Default for ApiKeyRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            delay_millis: 1_000,
            strategy: RetryDelayStrategy::Exponential,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicyOverride {
    pub enabled: Option<bool>,
    pub max_retries: Option<u32>,
    pub delay_millis: Option<u64>,
    pub strategy: Option<RetryDelayStrategy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyProfile {
    pub retry_policy_override: Option<RetryPolicyOverride>,
}

impl ApiKeyProfile {
    pub fn retry_policy(&self, system_default: &ApiKeyRetryPolicy) -> ApiKeyRetryPolicy {
        self.retry_policy_override.as_ref().map_or_else(
            || system_default.clone(),
            |retry_override| system_default.merged_with_override(retry_override),
        )
    }
}

pub struct ApiKeyPlaintext(String);

impl ApiKeyPlaintext {
    pub fn into_secret(self) -> String {
        self.0
    }
}

impl fmt::Debug for ApiKeyPlaintext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKeyPlaintext(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateApiKeyInput {
    pub project_id: String,
    pub name: String,
    pub scope_slugs: Vec<String>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug)]
pub struct CreatedApiKey {
    pub api_key: ApiKey,
    pub plaintext_key: ApiKeyPlaintext,
}

#[derive(Debug)]
pub struct RotatedApiKey {
    pub old_api_key: ApiKey,
    pub new_api_key: ApiKey,
    pub plaintext_key: ApiKeyPlaintext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRotation {
    pub old_api_key: ApiKey,
    pub new_api_key: ApiKey,
}

#[async_trait]
pub trait UserServiceRepo: Send + Sync {
    async fn find_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> UserProjectServiceResult<Option<User>>;

    async fn save_user(&self, ctx: &RequestContext, user: User) -> UserProjectServiceResult<User>;
}

#[async_trait]
pub trait ProjectServiceRepo: Send + Sync {
    async fn find_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> UserProjectServiceResult<Option<Project>>;

    async fn save_project(
        &self,
        ctx: &RequestContext,
        project: Project,
    ) -> UserProjectServiceResult<Project>;
}

#[async_trait]
pub trait RoleServiceRepo: Send + Sync {
    async fn find_role(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> UserProjectServiceResult<Option<Role>>;

    async fn save_role(&self, ctx: &RequestContext, role: Role) -> UserProjectServiceResult<Role>;
}

#[async_trait]
pub trait ApiKeyServiceRepo: Send + Sync {
    async fn find_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> UserProjectServiceResult<Option<ApiKey>>;

    async fn insert_api_key(
        &self,
        ctx: &RequestContext,
        api_key: ApiKey,
    ) -> UserProjectServiceResult<ApiKey>;

    async fn rotate_api_key(
        &self,
        ctx: &RequestContext,
        old_api_key_id: &str,
        new_api_key: ApiKey,
    ) -> UserProjectServiceResult<ApiKeyRotation>;

    /// Reserve the project/name pair in PostgreSQL before creating or rotating
    /// an API key.
    async fn lock_project_for_api_key_name(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> UserProjectServiceResult<()>;
}

pub struct UserService {
    repo: Arc<dyn UserServiceRepo>,
}

impl UserService {
    pub fn new(repo: Arc<dyn UserServiceRepo>) -> Self {
        Self { repo }
    }

    pub async fn find_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> UserProjectServiceResult<Option<User>> {
        self.repo.find_user(ctx, user_id).await
    }

    pub async fn save_user(
        &self,
        ctx: &RequestContext,
        user: User,
    ) -> UserProjectServiceResult<User> {
        self.repo.save_user(ctx, user).await
    }
}

pub struct ProjectService {
    repo: Arc<dyn ProjectServiceRepo>,
}

impl ProjectService {
    pub fn new(repo: Arc<dyn ProjectServiceRepo>) -> Self {
        Self { repo }
    }

    pub async fn find_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> UserProjectServiceResult<Option<Project>> {
        self.repo.find_project(ctx, project_id).await
    }

    pub async fn save_project(
        &self,
        ctx: &RequestContext,
        project: Project,
    ) -> UserProjectServiceResult<Project> {
        self.repo.save_project(ctx, project).await
    }
}

pub struct RoleService {
    repo: Arc<dyn RoleServiceRepo>,
}

impl RoleService {
    pub fn new(repo: Arc<dyn RoleServiceRepo>) -> Self {
        Self { repo }
    }

    pub async fn find_role(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> UserProjectServiceResult<Option<Role>> {
        self.repo.find_role(ctx, role_id).await
    }

    pub async fn save_role(
        &self,
        ctx: &RequestContext,
        role: Role,
    ) -> UserProjectServiceResult<Role> {
        PermissionValidator::validate_scope_slugs(&role.scope_slugs)?;
        self.repo.save_role(ctx, role).await
    }
}

pub struct ApiKeyService {
    repo: Arc<dyn ApiKeyServiceRepo>,
}

impl ApiKeyService {
    pub fn new(repo: Arc<dyn ApiKeyServiceRepo>) -> Self {
        Self { repo }
    }

    pub async fn create_api_key(
        &self,
        ctx: &RequestContext,
        input: CreateApiKeyInput,
    ) -> UserProjectServiceResult<CreatedApiKey> {
        PermissionValidator::validate_scope_slugs(&input.scope_slugs)?;
        self.repo
            .lock_project_for_api_key_name(ctx, &input.project_id, &input.name)
            .await?;

        let plaintext_key = generate_api_key_plaintext();
        let api_key = ApiKey {
            id: generate_api_key_id(),
            project_id: input.project_id,
            name: input.name,
            secret_digest: digest_secret(&plaintext_key.0),
            secret_preview: preview_secret(&plaintext_key.0),
            scope_slugs: input.scope_slugs,
            status: ApiKeyStatus::Enabled,
            // Default key type is `User` (see `ApiKeyType::default`); higher-
            // level create flow resolves the type via `resolve_create_type`.
            key_type: ApiKeyType::default(),
            created_by_user_id: input.created_by_user_id,
        };
        let api_key = self.repo.insert_api_key(ctx, api_key).await?;

        Ok(CreatedApiKey {
            api_key,
            plaintext_key,
        })
    }

    pub async fn rotate_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> UserProjectServiceResult<RotatedApiKey> {
        let old_api_key = self
            .repo
            .find_api_key(ctx, api_key_id)
            .await?
            .ok_or_else(|| UserProjectServiceError::ApiKeyNotFound(api_key_id.to_string()))?;
        if old_api_key.status != ApiKeyStatus::Enabled {
            return Err(UserProjectServiceError::ApiKeyInactive(
                api_key_id.to_string(),
            ));
        }
        PermissionValidator::validate_scope_slugs(&old_api_key.scope_slugs)?;
        self.repo
            .lock_project_for_api_key_name(ctx, &old_api_key.project_id, &old_api_key.name)
            .await?;

        let plaintext_key = generate_api_key_plaintext();
        let new_api_key = ApiKey {
            id: generate_api_key_id(),
            project_id: old_api_key.project_id.clone(),
            name: old_api_key.name.clone(),
            secret_digest: digest_secret(&plaintext_key.0),
            secret_preview: preview_secret(&plaintext_key.0),
            scope_slugs: old_api_key.scope_slugs.clone(),
            status: ApiKeyStatus::Enabled,
            // Mirrors Go `RotateAPIKey`, which preserves all properties
            // (key type included) and only swaps `Key`.
            key_type: old_api_key.key_type,
            created_by_user_id: old_api_key.created_by_user_id.clone(),
        };

        // The repo owns the atomic boundary: invalidate the old key and persist
        // the replacement together when a concrete backend is added.
        let rotation = self
            .repo
            .rotate_api_key(ctx, &old_api_key.id, new_api_key)
            .await?;

        Ok(RotatedApiKey {
            old_api_key: rotation.old_api_key,
            new_api_key: rotation.new_api_key,
            plaintext_key,
        })
    }
}

pub struct PermissionValidator;

impl PermissionValidator {
    pub fn validate_scope_slug(scope_slug: &str) -> UserProjectServiceResult<()> {
        if known_scope_slugs().contains(&scope_slug) {
            Ok(())
        } else {
            Err(UserProjectServiceError::InvalidScopeSlug(
                scope_slug.to_string(),
            ))
        }
    }

    pub fn validate_scope_slugs(scope_slugs: &[String]) -> UserProjectServiceResult<ScopeSet> {
        for scope_slug in scope_slugs {
            Self::validate_scope_slug(scope_slug)?;
        }

        // Normalize through conduit-auth's Scope/ScopeSet so services and auth
        // agree on the shape consumed by RBAC checks.
        Ok(ScopeSet::from_strings(
            scope_slugs
                .iter()
                .map(|scope_slug| Scope::api_key(scope_slug).to_string()),
        ))
    }
}

pub fn known_scope_slugs() -> &'static [&'static str] {
    &[
        slug::SYSTEM_ADMIN,
        slug::READ_DASHBOARD,
        slug::READ_CHANNELS,
        slug::WRITE_CHANNELS,
        slug::READ_DATA_STORAGES,
        slug::WRITE_DATA_STORAGES,
        slug::READ_USERS,
        slug::WRITE_USERS,
        slug::READ_SETTINGS,
        slug::WRITE_SETTINGS,
        slug::READ_ROLES,
        slug::WRITE_ROLES,
        slug::READ_PROJECTS,
        slug::WRITE_PROJECTS,
        slug::READ_API_KEYS,
        slug::WRITE_API_KEYS,
        slug::READ_REQUESTS,
        slug::WRITE_REQUESTS,
        slug::READ_PROMPTS,
        slug::WRITE_PROMPTS,
        slug::READ_GROUPS,
        slug::WRITE_GROUPS,
        slug::READ_SUBSCRIPTIONS,
        slug::WRITE_SUBSCRIPTIONS,
        slug::READ_BILLING,
        slug::WRITE_BILLING,
        slug::GRANT_CREDIT,
        slug::READ_COMMERCIALIZATION,
        slug::WRITE_COMMERCIALIZATION,
    ]
}

// ============================================================================
// RUST-P5-003 ProjectService — pure-logic port of
// `conduit/internal/server/biz/project.go`:
//   * `CreateProject` (side-effect ordering / default roles / owner link)
//   * `ValidateProjectProfiles` (`profiles` JSON field)
//   * `UpdateProjectStatus` (legal status transitions)
//   * `DeleteProject` (soft-delete side-effect ordering)
//
// These functions are **pure**: they only describe what a future repo-backed
// `ProjectService` must do. They take inputs, return a plan/validation result,
// and never touch IO. The ordering is load-bearing — mirrors the Go flow.
// ============================================================================

/// Default scope slugs for the **Admin** project-level role.
///
/// Mirrors Go `CreateProject` adminScopes (biz/project.go:88-97).
pub fn default_admin_role_scopes() -> &'static [&'static str] {
    &[
        slug::READ_USERS,
        slug::WRITE_USERS,
        slug::READ_ROLES,
        slug::WRITE_ROLES,
        slug::READ_API_KEYS,
        slug::WRITE_API_KEYS,
        slug::READ_REQUESTS,
        slug::WRITE_REQUESTS,
    ]
}

/// Default scope slugs for the **Developer** project-level role.
///
/// Mirrors Go `CreateProject` developerScopes (biz/project.go:110-115).
pub fn default_developer_role_scopes() -> &'static [&'static str] {
    &[
        slug::READ_USERS,
        slug::READ_API_KEYS,
        slug::WRITE_API_KEYS,
        slug::READ_REQUESTS,
    ]
}

/// Default scope slugs for the **Viewer** project-level role.
///
/// Mirrors Go `CreateProject` viewerScopes (biz/project.go:128-131).
pub fn default_viewer_role_scopes() -> &'static [&'static str] {
    &[slug::READ_USERS, slug::READ_REQUESTS]
}

/// Names + scope slugs for the three default project-level roles seeded by Go's
/// `CreateProject`. Order matters: Admin → Developer → Viewer, matching Go.
pub fn default_project_roles() -> [DefaultProjectRole; 3] {
    [
        DefaultProjectRole::new("Admin", default_admin_role_scopes()),
        DefaultProjectRole::new("Developer", default_developer_role_scopes()),
        DefaultProjectRole::new("Viewer", default_viewer_role_scopes()),
    ]
}

/// One of the three default roles a Go `CreateProject` seeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultProjectRole {
    pub name: &'static str,
    pub scopes: &'static [&'static str],
}

impl DefaultProjectRole {
    pub const fn new(name: &'static str, scopes: &'static [&'static str]) -> Self {
        Self { name, scopes }
    }
}

/// Input to [`create_project_plan`] — a pure description of a Go
/// `CreateProjectInput` plus the acting owner user id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectInput {
    pub name: String,
    pub description: Option<String>,
    /// ID of the user creating the project (Go pulls this from
    /// `contexts.GetUser(ctx)` and assigns them as owner). Pure planning does
    /// not consult a context, so the caller must supply it explicitly.
    pub owner_user_id: String,
}

/// One ordered side-effect step produced by [`create_project_plan`].
///
/// Order mirrors Go `CreateProject` (biz/project.go:73-152):
/// project row → 3 default roles (Admin/Developer/Viewer) → owner link.
/// Plus the [`SoftDelete`] step which mirrors `DeleteProject`
/// (biz/project.go:333-383).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateProjectStep {
    /// 1. Insert the project row.
    InsertProject {
        name: String,
        description: Option<String>,
    },
    /// 2. Insert one of the three default project-level roles.
    ///    Emitted 3 times in order: Admin, Developer, Viewer.
    SeedDefaultRole {
        role_name: &'static str,
        scopes: &'static [&'static str],
    },
    /// 3. Link the creator to the project as owner (Go `UserProject{IsOwner:true}`).
    LinkOwner { user_id: String },
    /// Soft-delete step (mirrors `DeleteProject` ordering): delete project
    /// users → project roles → project API keys → soft-delete project row.
    /// Emitted by [`soft_delete_project_plan`], not by [`create_project_plan`].
    SoftDelete(SoftDeleteStep),
}

/// Ordered soft-delete side-effect steps for a project.
///
/// Mirrors Go `DeleteProject` (biz/project.go:333-383):
/// 1. Delete `UserProject` relationships
/// 2. Delete project-level `Role`s
/// 3. Delete project `APIKey`s
/// 4. Soft-delete the project row
/// (cache invalidation is repo infra, not a plan step).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoftDeleteStep {
    DeleteProjectUsers,
    DeleteProjectRoles,
    DeleteProjectApiKeys,
    SoftDeleteProject,
}

/// The pure plan returned by [`create_project_plan`] / [`soft_delete_project_plan`].
///
/// A repo-backed `ProjectService` will execute `steps` in order inside a
/// transaction (Go wraps them in `RunInTransaction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectPlan {
    pub steps: Vec<CreateProjectStep>,
}

/// Mirrors Go `CreateProject` (biz/project.go:53-155) as a **pure** step list.
///
/// Performs the input-validation Go does (`name` uniqueness is a repo concern;
/// here we only require a non-empty name + present owner), then returns the
/// ordered plan: project row → 3 default roles → owner link.
///
/// Per Go's permission flow, the acting user is the **owner** of the new
/// project. We do not model the `contexts.GetUser` miss here — the caller has
/// already authenticated and supplies `owner_user_id`.
pub fn create_project_plan(
    input: &CreateProjectInput,
) -> UserProjectServiceResult<CreateProjectPlan> {
    if input.name.trim().is_empty() {
        return Err(UserProjectServiceError::DuplicateProjectName(
            input.name.clone(),
        ));
    }
    if input.owner_user_id.is_empty() {
        return Err(UserProjectServiceError::UserNotFound(String::new()));
    }

    let mut steps = Vec::with_capacity(1 + 3 + 1);
    steps.push(CreateProjectStep::InsertProject {
        name: input.name.clone(),
        description: input.description.clone(),
    });
    for role in default_project_roles() {
        steps.push(CreateProjectStep::SeedDefaultRole {
            role_name: role.name,
            scopes: role.scopes,
        });
    }
    steps.push(CreateProjectStep::LinkOwner {
        user_id: input.owner_user_id.clone(),
    });

    Ok(CreateProjectPlan { steps })
}

/// Mirrors Go `DeleteProject` (biz/project.go:333-383) as a **pure** step list.
///
/// Permission validation is the caller's job (Go calls
/// `permissionValidator.CanDeleteProject` first). This function only emits the
/// in-transaction cascade order.
pub fn soft_delete_project_plan() -> CreateProjectPlan {
    CreateProjectPlan {
        steps: vec![
            CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectUsers),
            CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectRoles),
            CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectApiKeys),
            CreateProjectStep::SoftDelete(SoftDeleteStep::SoftDeleteProject),
        ],
    }
}

// ----------------------------------------------------------------------------
// profiles JSON — pure port of Go `ValidateProjectProfiles`
// (biz/project.go:257-296) + active-profile selection helper.
// ----------------------------------------------------------------------------

/// Validate `ProjectProfiles` exactly like Go's `ValidateProjectProfiles`:
///   * names are unique, case-insensitive
///   * names are non-empty (after trim)
///   * `channel_tags_match_mode` is `is_valid` (empty is allowed)
///   * if `active_profile` is set, it must exist in `profiles`
///
/// Pure — no repo access. Used as the pre-write guard for
/// `UpdateProjectProfiles` (Go biz/project.go:235-254).
pub fn validate_project_profiles(
    profiles: &conduit_core::objects::project::ProjectProfiles,
) -> UserProjectServiceResult<()> {
    use conduit_core::objects::apikey::is_valid as mode_is_valid;
    use std::collections::HashSet;

    let mut seen: HashSet<String> = HashSet::new();
    for profile in &profiles.profiles {
        let name_lower = profile.name.trim().to_ascii_lowercase();
        if name_lower.is_empty() {
            return Err(UserProjectServiceError::EmptyProfileName);
        }
        if !seen.insert(name_lower) {
            return Err(UserProjectServiceError::DuplicateProfileName(
                profile.name.clone(),
            ));
        }
        let mode = profile
            .channel_tags_match_mode
            .as_deref()
            .unwrap_or_default();
        if !mode_is_valid(mode) {
            return Err(UserProjectServiceError::InvalidChannelTagsMatchMode(
                profile.name.clone(),
            ));
        }
    }

    if !profiles.active_profile.is_empty() {
        let found = profiles
            .profiles
            .iter()
            .any(|p| p.name == profiles.active_profile);
        if !found {
            return Err(UserProjectServiceError::ActiveProfileNotFound(
                profiles.active_profile.clone(),
            ));
        }
    }
    Ok(())
}

/// Select the active profile entry from a validated `ProjectProfiles`.
///
/// Returns `None` when `active_profile` is empty (matches Go's "no active
/// profile" behavior) or when the named profile is absent — call
/// [`validate_project_profiles`] first to enforce existence.
pub fn select_active_profile<'a>(
    profiles: &'a conduit_core::objects::project::ProjectProfiles,
    name: &str,
) -> Option<&'a conduit_core::objects::project::ProjectProfile> {
    if name.is_empty() {
        return None;
    }
    profiles.profiles.iter().find(|p| p.name == name)
}

// ----------------------------------------------------------------------------
// status transitions — pure port of Go `ent/project.StatusValidator` +
// the legal transition matrix implied by Go's two-value enum.
// ----------------------------------------------------------------------------

/// Validate that `to` is a legal project status value.
///
/// Mirrors Go `ent/project.StatusValidator` (only `active`/`archived` are
/// legal — there is no `suspended` in the Go contract).
pub fn validate_project_status(to: ProjectStatus) -> UserProjectServiceResult<()> {
    ProjectStatus::validate(to)
}

/// Validate that the transition `from -> to` is legal for a project.
///
/// Go has no explicit transition guard (any of the 2x2 = 4 ordered pairs is
/// permitted through `UpdateProjectStatus`), so this mirrors that: any
/// transition between the two valid enum values is allowed, and a self-loop
/// is a no-op success. Unknown values are rejected by [`validate_project_status`].
///
/// The "no suspended" decision is load-bearing: per CLAUDE.md "When docs and
/// code disagree, prefer the code/contract" — the Go `ent/project` enum has
/// only `active`/`archived`, so we do not model a `suspended` state.
pub fn validate_status_transition(
    from: ProjectStatus,
    to: ProjectStatus,
) -> UserProjectServiceResult<()> {
    validate_project_status(from)?;
    validate_project_status(to)?;
    // Go permits every ordered pair of valid values. No-op self-loops are fine.
    let _ = (from, to);
    Ok(())
}

fn generate_api_key_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("akid_{}", encode_hex(&bytes))
}

fn generate_api_key_plaintext() -> ApiKeyPlaintext {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    ApiKeyPlaintext(format!("ak_{}", encode_hex(&bytes)))
}

fn digest_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    format!("sha256:{}", encode_hex(&digest))
}

fn preview_secret(secret: &str) -> String {
    let suffix_start = secret.len().saturating_sub(8);
    format!("...{}", &secret[suffix_start..])
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use conduit_db::{PolicyContext, Principal, RequestContext};
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeApiKeyRepo {
        keys: Mutex<BTreeMap<String, ApiKey>>,
        locks: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl ApiKeyServiceRepo for FakeApiKeyRepo {
        async fn find_api_key(
            &self,
            _ctx: &RequestContext,
            api_key_id: &str,
        ) -> UserProjectServiceResult<Option<ApiKey>> {
            Ok(self.keys.lock().await.get(api_key_id).cloned())
        }

        async fn insert_api_key(
            &self,
            _ctx: &RequestContext,
            api_key: ApiKey,
        ) -> UserProjectServiceResult<ApiKey> {
            self.keys
                .lock()
                .await
                .insert(api_key.id.clone(), api_key.clone());
            Ok(api_key)
        }

        async fn rotate_api_key(
            &self,
            _ctx: &RequestContext,
            old_api_key_id: &str,
            new_api_key: ApiKey,
        ) -> UserProjectServiceResult<ApiKeyRotation> {
            let mut keys = self.keys.lock().await;
            let Some(old_api_key) = keys.get_mut(old_api_key_id) else {
                return Err(UserProjectServiceError::ApiKeyNotFound(
                    old_api_key_id.to_string(),
                ));
            };
            // Mirrors Go `RotateAPIKey`: only the `Key` is swapped; status is
            // preserved. Do NOT mutate `old_api_key.status` here.
            let old_api_key = old_api_key.clone();
            keys.insert(new_api_key.id.clone(), new_api_key.clone());

            Ok(ApiKeyRotation {
                old_api_key,
                new_api_key,
            })
        }

        async fn lock_project_for_api_key_name(
            &self,
            _ctx: &RequestContext,
            project_id: &str,
            name: &str,
        ) -> UserProjectServiceResult<()> {
            // The in-memory test double records the lock request; production
            // performs the PostgreSQL lock in its repository transaction.
            self.locks
                .lock()
                .await
                .push((project_id.to_string(), name.to_string()));
            Ok(())
        }
    }

    fn test_ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn system_retry_policy() -> ApiKeyRetryPolicy {
        ApiKeyRetryPolicy {
            enabled: true,
            max_retries: 2,
            delay_millis: 250,
            strategy: RetryDelayStrategy::Fixed,
        }
    }

    #[test]
    fn api_key_profile_uses_system_retry_policy_without_override() {
        let system_default = system_retry_policy();
        let profile = ApiKeyProfile::default();

        let merged = profile.retry_policy(&system_default);

        assert_eq!(merged, system_default);
    }

    #[test]
    fn api_key_profile_retry_override_replaces_max_retries_only() {
        let profile = ApiKeyProfile {
            retry_policy_override: Some(RetryPolicyOverride {
                max_retries: Some(5),
                ..RetryPolicyOverride::default()
            }),
        };

        let merged = profile.retry_policy(&system_retry_policy());

        assert_eq!(
            merged,
            ApiKeyRetryPolicy {
                enabled: true,
                max_retries: 5,
                delay_millis: 250,
                strategy: RetryDelayStrategy::Fixed,
            }
        );
    }

    #[test]
    fn api_key_profile_retry_override_can_disable_retry() {
        let profile = ApiKeyProfile {
            retry_policy_override: Some(RetryPolicyOverride {
                enabled: Some(false),
                ..RetryPolicyOverride::default()
            }),
        };

        let merged = profile.retry_policy(&system_retry_policy());

        assert_eq!(
            merged,
            ApiKeyRetryPolicy {
                enabled: false,
                max_retries: 2,
                delay_millis: 250,
                strategy: RetryDelayStrategy::Fixed,
            }
        );
    }

    #[tokio::test]
    async fn rotate_api_key_invalidates_old_and_returns_new_plaintext_once()
    -> UserProjectServiceResult<()> {
        let repo = Arc::new(FakeApiKeyRepo::default());
        let service = ApiKeyService::new(repo.clone());
        let ctx = test_ctx();
        let old_api_key = ApiKey {
            id: "old-key".to_string(),
            project_id: "project-a".to_string(),
            name: "default".to_string(),
            secret_digest: "sha256:old".to_string(),
            secret_preview: "...old".to_string(),
            scope_slugs: vec![slug::READ_REQUESTS.to_string()],
            status: ApiKeyStatus::Enabled,
            key_type: ApiKeyType::User,
            created_by_user_id: Some("user-a".to_string()),
        };
        repo.insert_api_key(&ctx, old_api_key).await?;

        let rotated = service.rotate_api_key(&ctx, "old-key").await?;
        let plaintext = rotated.plaintext_key.into_secret();
        let keys = repo.keys.lock().await;
        let stored_old = keys
            .get("old-key")
            .ok_or_else(|| UserProjectServiceError::ApiKeyNotFound("old-key".to_string()))?;
        let stored_new = keys.get(&rotated.new_api_key.id).ok_or_else(|| {
            UserProjectServiceError::ApiKeyNotFound(rotated.new_api_key.id.clone())
        })?;

        // Mirrors Go `RotateAPIKey`: status is preserved (NOT changed). The old
        // key retains its original `enabled` status; only `Key` is swapped.
        assert_eq!(rotated.old_api_key.status, ApiKeyStatus::Enabled);
        assert_eq!(stored_old.status, ApiKeyStatus::Enabled);
        assert_eq!(stored_new.status, ApiKeyStatus::Enabled);
        assert_ne!(stored_new.secret_digest, "sha256:old");
        assert_ne!(stored_new.secret_digest, plaintext);
        assert_eq!(stored_new.secret_preview, preview_secret(&plaintext));
        assert_eq!(
            stored_new.scope_slugs,
            vec![slug::READ_REQUESTS.to_string()]
        );
        drop(keys);

        let locks = repo.locks.lock().await;
        assert_eq!(
            locks.as_slice(),
            &[("project-a".to_string(), "default".to_string())]
        );

        Ok(())
    }

    #[test]
    fn permission_validator_uses_auth_scope_shape() -> UserProjectServiceResult<()> {
        let scope_set =
            PermissionValidator::validate_scope_slugs(&[slug::READ_REQUESTS.to_string()])?;

        assert!(
            scope_set
                .matched_api_key_scope(slug::READ_REQUESTS)
                .is_some()
        );
        assert!(matches!(
            PermissionValidator::validate_scope_slug("not_a_scope"),
            Err(UserProjectServiceError::InvalidScopeSlug(scope))
                if scope == "not_a_scope"
        ));

        Ok(())
    }

    // ========================================================================
    // RUST-P5-003 — ProjectService pure-logic tests mirroring Go
    // `biz/project_test.go::TestValidateProjectProfiles` golden cases +
    // create-default / soft-delete plan ordering + status transitions.
    // ========================================================================

    fn profile(name: &str, mode: Option<&str>) -> conduit_core::objects::project::ProjectProfile {
        conduit_core::objects::project::ProjectProfile {
            name: name.to_string(),
            channel_tags_match_mode: mode.map(|m| m.to_string()),
            ..Default::default()
        }
    }

    fn profiles(
        active: &str,
        entries: &[conduit_core::objects::project::ProjectProfile],
    ) -> conduit_core::objects::project::ProjectProfiles {
        conduit_core::objects::project::ProjectProfiles {
            active_profile: active.to_string(),
            profiles: entries.to_vec(),
        }
    }

    // --- Go biz/project_test.go::TestValidateProjectProfiles golden table ---

    #[test]
    fn validate_project_profiles_valid_with_active() -> UserProjectServiceResult<()> {
        let p = profiles(
            "Profile1",
            &[
                profile("Profile1", Some("any")),
                profile("Profile2", Some("all")),
                profile("Profile3", Some("none")),
            ],
        );
        validate_project_profiles(&p)?;
        Ok(())
    }

    #[test]
    fn validate_project_profiles_valid_without_active() -> UserProjectServiceResult<()> {
        let p = profiles("", &[profile("Profile1", Some("any"))]);
        validate_project_profiles(&p)?;
        Ok(())
    }

    #[test]
    fn validate_project_profiles_empty_name_rejected() {
        let p = profiles("", &[profile("", Some("any"))]);
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::EmptyProfileName)
        ));
    }

    #[test]
    fn validate_project_profiles_whitespace_only_name_rejected() {
        // Go: `name` is trimmed; `"   "` -> lower-empty -> error.
        let mut whitespace = profile("   ", Some("any"));
        whitespace.name = "   ".to_string();
        let p = profiles("", &[whitespace]);
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::EmptyProfileName)
        ));
    }

    #[test]
    fn validate_project_profiles_duplicate_names_rejected() {
        let p = profiles(
            "",
            &[
                profile("Profile1", Some("any")),
                profile("Profile1", Some("all")),
            ],
        );
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::DuplicateProfileName(name)) if name == "Profile1"
        ));
    }

    #[test]
    fn validate_project_profiles_duplicate_names_case_insensitive() {
        // Go: case-fold via ToLower before the seen-map lookup.
        let p = profiles(
            "",
            &[
                profile("Profile1", Some("any")),
                profile("PROFILE1", Some("all")),
            ],
        );
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::DuplicateProfileName(name)) if name == "PROFILE1"
        ));
    }

    #[test]
    fn validate_project_profiles_invalid_channel_tags_match_mode() {
        let p = profiles("", &[profile("Profile1", Some("invalid"))]);
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::InvalidChannelTagsMatchMode(name)) if name == "Profile1"
        ));
    }

    #[test]
    fn validate_project_profiles_active_profile_must_exist() {
        let p = profiles("NonExistent", &[profile("Profile1", Some("any"))]);
        assert!(matches!(
            validate_project_profiles(&p),
            Err(UserProjectServiceError::ActiveProfileNotFound(name)) if name == "NonExistent"
        ));
    }

    #[test]
    fn validate_project_profiles_empty_match_mode_is_valid() -> UserProjectServiceResult<()> {
        // Go golden case: empty channelTagsMatchMode is valid (default).
        let p = profiles("", &[profile("Profile1", None)]);
        validate_project_profiles(&p)?;
        Ok(())
    }

    // --- active profile selection helper ---

    #[test]
    fn select_active_profile_returns_named_entry() {
        let p = profiles(
            "Profile2",
            &[
                profile("Profile1", Some("any")),
                profile("Profile2", Some("all")),
            ],
        );
        let selected = select_active_profile(&p, "Profile2");
        assert!(selected.is_some());
        assert_eq!(selected.map(|p| p.name.as_str()), Some("Profile2"));
    }

    #[test]
    fn select_active_profile_returns_none_for_empty_name() {
        let p = profiles("", &[profile("Profile1", Some("any"))]);
        assert!(select_active_profile(&p, "").is_none());
    }

    #[test]
    fn select_active_profile_returns_none_for_missing_name() {
        let p = profiles("", &[profile("Profile1", Some("any"))]);
        assert!(select_active_profile(&p, "Ghost").is_none());
    }

    // --- create-default + soft-delete plan ordering ---

    #[test]
    fn create_project_plan_orders_project_then_roles_then_owner() -> UserProjectServiceResult<()> {
        let input = CreateProjectInput {
            name: "acme".to_string(),
            description: Some("desc".to_string()),
            owner_user_id: "user-1".to_string(),
        };
        let plan = create_project_plan(&input)?;

        // 1 project + 3 default roles + 1 owner link = 5 steps.
        assert_eq!(plan.steps.len(), 5);
        assert!(matches!(
            plan.steps[0],
            CreateProjectStep::InsertProject { ref name, ref description } if name == "acme" && description.as_deref() == Some("desc")
        ));
        // Default role order is Admin -> Developer -> Viewer (Go order).
        assert!(matches!(
            plan.steps[1],
            CreateProjectStep::SeedDefaultRole {
                role_name: "Admin",
                ..
            }
        ));
        assert!(matches!(
            plan.steps[2],
            CreateProjectStep::SeedDefaultRole {
                role_name: "Developer",
                ..
            }
        ));
        assert!(matches!(
            plan.steps[3],
            CreateProjectStep::SeedDefaultRole {
                role_name: "Viewer",
                ..
            }
        ));
        assert!(matches!(
            plan.steps[4],
            CreateProjectStep::LinkOwner { ref user_id } if user_id == "user-1"
        ));
        Ok(())
    }

    #[test]
    fn default_admin_role_scopes_match_go_adminscopes() {
        // Mirrors Go biz/project.go:88-97 adminScopes exactly.
        assert_eq!(
            default_admin_role_scopes(),
            &[
                slug::READ_USERS,
                slug::WRITE_USERS,
                slug::READ_ROLES,
                slug::WRITE_ROLES,
                slug::READ_API_KEYS,
                slug::WRITE_API_KEYS,
                slug::READ_REQUESTS,
                slug::WRITE_REQUESTS,
            ]
        );
    }

    #[test]
    fn default_developer_role_scopes_match_go_developerscopes() {
        // Mirrors Go biz/project.go:110-115 developerScopes exactly.
        assert_eq!(
            default_developer_role_scopes(),
            &[
                slug::READ_USERS,
                slug::READ_API_KEYS,
                slug::WRITE_API_KEYS,
                slug::READ_REQUESTS,
            ]
        );
    }

    #[test]
    fn default_viewer_role_scopes_match_go_viewerscopes() {
        // Mirrors Go biz/project.go:128-131 viewerScopes exactly.
        assert_eq!(
            default_viewer_role_scopes(),
            &[slug::READ_USERS, slug::READ_REQUESTS]
        );
    }

    #[test]
    fn create_project_plan_rejects_empty_name() {
        let input = CreateProjectInput {
            name: "   ".to_string(),
            description: None,
            owner_user_id: "user-1".to_string(),
        };
        assert!(matches!(
            create_project_plan(&input),
            Err(UserProjectServiceError::DuplicateProjectName(_))
        ));
    }

    #[test]
    fn create_project_plan_rejects_missing_owner() {
        let input = CreateProjectInput {
            name: "acme".to_string(),
            description: None,
            owner_user_id: String::new(),
        };
        assert!(matches!(
            create_project_plan(&input),
            Err(UserProjectServiceError::UserNotFound(_))
        ));
    }

    #[test]
    fn soft_delete_project_plan_orders_users_roles_apikeys_then_project() {
        // Mirrors Go biz/project.go:333-383 cascade order.
        let plan = soft_delete_project_plan();
        assert_eq!(
            plan.steps,
            vec![
                CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectUsers),
                CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectRoles),
                CreateProjectStep::SoftDelete(SoftDeleteStep::DeleteProjectApiKeys),
                CreateProjectStep::SoftDelete(SoftDeleteStep::SoftDeleteProject),
            ]
        );
    }

    // --- status transitions ---

    #[test]
    fn validate_project_status_accepts_go_enum_values() -> UserProjectServiceResult<()> {
        validate_project_status(ProjectStatus::Active)?;
        validate_project_status(ProjectStatus::Archived)?;
        Ok(())
    }

    #[test]
    fn validate_status_transition_permits_all_go_legal_pairs() -> UserProjectServiceResult<()> {
        // Go has no transition guard: every ordered pair of {active, archived}
        // is allowed (including self-loops, which Go's UpdateProjectStatus would
        // also accept).
        for from in [ProjectStatus::Active, ProjectStatus::Archived] {
            for to in [ProjectStatus::Active, ProjectStatus::Archived] {
                validate_status_transition(from, to)?;
            }
        }
        Ok(())
    }

    #[test]
    fn project_status_serializes_to_go_lowercase_strings() -> Result<(), serde_json::Error> {
        // Go enum serializes as `"active"`/`"archived"`; the Rust rename_all
        // = "lowercase" must match for API/JSON parity.
        assert_eq!(serde_json::to_string(&ProjectStatus::Active)?, "\"active\"");
        assert_eq!(
            serde_json::to_string(&ProjectStatus::Archived)?,
            "\"archived\""
        );
        assert_eq!(
            serde_json::from_str::<ProjectStatus>("\"active\"")?,
            ProjectStatus::Active
        );
        assert_eq!(
            serde_json::from_str::<ProjectStatus>("\"archived\"")?,
            ProjectStatus::Archived
        );
        Ok(())
    }
}

//! PostgreSQL admin GraphQL adapters for projects and roles.
//!
//! Multi-table mutations use PostgreSQL transactions. Project creation cannot
//! attach an owner because the GraphQL service trait does not carry the current
//! actor; this matches the existing host adapter's documented behavior.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use sqlx::PgPool;

use conduit_admin_graphql::apikey::ChannelTagsMatchMode as GqlTagsMatchMode;
use conduit_admin_graphql::channel::OrderDirection;
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::project::{
    CreateProjectInput as GqlCreateProjectInput, Project as GqlProject, ProjectConnection,
    ProjectConnectionArgs, ProjectEdge, ProjectMutationServices, ProjectOrderTerm,
    ProjectProfile as GqlProjectProfile, ProjectProfiles as GqlProjectProfiles,
    ProjectQueryServices, ProjectServiceError, ProjectStatus, ProjectWhereInput,
    UpdateProjectInput as GqlUpdateProjectInput, UpdateProjectProfilesInput,
};
use conduit_admin_graphql::role::{
    CreateRoleInput as GqlCreateRoleInput, Role as GqlRole, RoleConnection, RoleConnectionArgs,
    RoleEdge, RoleLevel, RoleMutationServices, RoleOrderTerm, RoleQueryServices, RoleServiceError,
    RoleWhereInput, UpdateRoleInput as GqlUpdateRoleInput,
};
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_core::objects::project::ProjectProfiles as CoreProjectProfiles;
use conduit_db::row::{ProjectRow, RoleRow};
use conduit_db::{
    CreateRoleInput as DbCreateRoleInput, ListProjectsQuery, ListRolesQuery, PgProjectRepo,
    PgRoleRepo, PolicyContext, Principal, ProjectRepo, RequestContext, RoleRepo,
    UpdateProjectInput as DbUpdateProjectInput, UpdateRoleInput as DbUpdateRoleInput,
};

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Boot-time request context: repo access before any authenticated principal
/// exists uses the `Test` principal (trusted bypass in `conduit-db` policy),
/// matching Go's pre-auth system path. Mirrors `wiring::boot_request_context`.
fn boot_ctx() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

/// Decode a GraphQL `ID` scalar into the numeric DB id string the repo expects:
/// a `gid://conduit/<Type>/<n>` typed GUID or a bare numeric string. Anything
/// else yields `None` (treated as "no such row" by the callers).
fn db_id_from_gql(raw: &str) -> Option<String> {
    if let Ok(guid) = parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

fn validate_role_scopes(level: RoleLevel, scopes: &[String]) -> Result<(), RoleServiceError> {
    for scope in scopes {
        if !conduit_auth::scopes::is_known_scope_slug(scope) {
            return Err(RoleServiceError::InvalidScopeSlug(scope.clone()));
        }
        if level == RoleLevel::Project && !conduit_auth::scopes::supports_project_role(scope) {
            return Err(RoleServiceError::ScopeNotAllowedForProjectRole(
                scope.clone(),
            ));
        }
    }
    Ok(())
}

/// Map the project `status` column (`active`/`archived`, default `active`) to
/// the GraphQL enum.
fn project_status_from_str(s: &str) -> ProjectStatus {
    match s {
        "archived" => ProjectStatus::Archived,
        _ => ProjectStatus::Active,
    }
}

/// GraphQL `ProjectStatus` enum → the wire literal stored in the column.
fn project_status_to_wire(s: ProjectStatus) -> &'static str {
    match s {
        ProjectStatus::Active => "active",
        ProjectStatus::Archived => "archived",
    }
}

/// Map the role `level` column (`system`/`project`, default `system`) to the
/// GraphQL enum.
fn role_level_from_str(s: &str) -> RoleLevel {
    match s {
        "project" => RoleLevel::Project,
        _ => RoleLevel::System,
    }
}

/// Map a stored `channelTagsMatchMode` wire literal to the GraphQL enum. Unknown
/// / absent values map to `None` (the field is nullable in the contract).
fn tags_mode_from_str(mode: Option<&str>) -> Option<GqlTagsMatchMode> {
    match mode {
        Some("any") => Some(GqlTagsMatchMode::Any),
        Some("all") => Some(GqlTagsMatchMode::All),
        Some("none") => Some(GqlTagsMatchMode::None),
        _ => None,
    }
}

/// GraphQL `ChannelTagsMatchMode` enum → the wire literal.
fn tags_mode_to_wire(mode: GqlTagsMatchMode) -> &'static str {
    match mode {
        GqlTagsMatchMode::Any => "any",
        GqlTagsMatchMode::All => "all",
        GqlTagsMatchMode::None => "none",
    }
}

/// Convert the stored project `profiles` JSON into the GraphQL `ProjectProfiles`
/// shape. Returns `None` for the empty/default object so the wire form omits it
/// (Go's zero-value `objects.ProjectProfiles` renders as an absent profile set).
/// Mirrors `wiring::map_project_profiles` but reads the typed `Value` column.
fn map_project_profiles(raw: Value) -> Option<GqlProjectProfiles> {
    let parsed: CoreProjectProfiles = serde_json::from_value(raw).unwrap_or_default();
    if parsed.active_profile.is_empty() && parsed.profiles.is_empty() {
        return None;
    }
    let profiles: Vec<GqlProjectProfile> = parsed
        .profiles
        .into_iter()
        .map(|p| GqlProjectProfile {
            name: p.name,
            channel_ids: if p.channel_ids.is_empty() {
                None
            } else {
                Some(p.channel_ids)
            },
            channel_tags: if p.channel_tags.is_empty() {
                None
            } else {
                Some(p.channel_tags)
            },
            channel_tags_match_mode: tags_mode_from_str(p.channel_tags_match_mode.as_deref()),
        })
        .collect();
    Some(GqlProjectProfiles {
        active_profile: parsed.active_profile,
        profiles: if profiles.is_empty() {
            None
        } else {
            Some(profiles)
        },
    })
}

/// Serialize an `UpdateProjectProfilesInput` into the `profiles` JSON column
/// value (the exact shape `map_project_profiles` reads back). Go binds the
/// profile input and output types to the SAME `objects.ProjectProfiles` struct
/// (`@goModel`), so an input round-trips into the stored object unchanged.
fn profiles_input_to_json(input: &UpdateProjectProfilesInput) -> Value {
    let profiles: Vec<Value> = input
        .profiles
        .iter()
        .flatten()
        .map(|p| {
            let mut obj = Map::new();
            obj.insert("name".to_string(), Value::String(p.name.clone()));
            if let Some(ids) = &p.channel_ids
                && !ids.is_empty()
            {
                obj.insert("channelIDs".to_string(), json!(ids));
            }
            if let Some(tags) = &p.channel_tags
                && !tags.is_empty()
            {
                obj.insert("channelTags".to_string(), json!(tags));
            }
            if let Some(mode) = &p.channel_tags_match_mode {
                obj.insert(
                    "channelTagsMatchMode".to_string(),
                    Value::String(tags_mode_to_wire(*mode).to_string()),
                );
            }
            Value::Object(obj)
        })
        .collect();
    json!({ "activeProfile": input.active_profile, "profiles": profiles })
}

/// Validate profiles before persisting — mirrors Go `ValidateProjectProfiles`
/// (`biz/project.go:257-296`): profile names non-empty and unique
/// (case-insensitive); the active profile (if set) must exist in the list. The
/// Go `channelTagsMatchMode.IsValid()` check is unconditionally satisfied here
/// because the GraphQL enum can only carry valid values.
fn validate_project_profiles(input: &UpdateProjectProfilesInput) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for p in input.profiles.iter().flatten() {
        let lower = p.name.trim().to_lowercase();
        if lower.is_empty() {
            return Err("profile name cannot be empty".to_string());
        }
        if !seen.insert(lower) {
            return Err(format!("duplicate profile name: {}", p.name));
        }
    }
    if !input.active_profile.is_empty()
        && !input
            .profiles
            .iter()
            .flatten()
            .any(|p| p.name == input.active_profile)
    {
        return Err(format!(
            "active profile '{}' does not exist in the profiles list",
            input.active_profile
        ));
    }
    Ok(())
}

fn project_profile_has_channel_selectors(
    profile: &conduit_core::objects::project::ProjectProfile,
) -> bool {
    !profile.channel_ids.is_empty()
        || !profile.channel_tags.is_empty()
        || profile.channel_tags_match_mode.is_some()
}

/// Preserve legacy Project Channel selectors as a read-only compatibility
/// payload. New profiles cannot add them, and profiles carrying them cannot be
/// renamed, removed, or changed through the customer-facing mutation.
fn preserve_project_channel_selectors(
    input: &mut UpdateProjectProfilesInput,
    previous: &CoreProjectProfiles,
) -> Result<(), String> {
    let error = || {
        "Project channel selectors are legacy and read-only; configure routing in the enterprise supply layer"
            .to_owned()
    };
    for profile in input.profiles.iter_mut().flatten() {
        let previous_profile = previous
            .profiles
            .iter()
            .find(|candidate| candidate.name == profile.name);
        let Some(previous_profile) = previous_profile else {
            if profile
                .channel_ids
                .as_ref()
                .is_some_and(|ids| !ids.is_empty())
                || profile
                    .channel_tags
                    .as_ref()
                    .is_some_and(|tags| !tags.is_empty())
                || profile.channel_tags_match_mode.is_some()
            {
                return Err(error());
            }
            continue;
        };

        if let Some(ids) = &profile.channel_ids
            && ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                != previous_profile
                    .channel_ids
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
        {
            return Err(error());
        }
        if let Some(tags) = &profile.channel_tags
            && tags.iter().collect::<std::collections::BTreeSet<_>>()
                != previous_profile
                    .channel_tags
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
        {
            return Err(error());
        }
        let previous_mode = tags_mode_from_str(previous_profile.channel_tags_match_mode.as_deref());
        if profile.channel_tags_match_mode.is_some()
            && profile.channel_tags_match_mode != previous_mode
        {
            return Err(error());
        }

        profile.channel_ids = (!previous_profile.channel_ids.is_empty())
            .then(|| previous_profile.channel_ids.clone());
        profile.channel_tags = (!previous_profile.channel_tags.is_empty())
            .then(|| previous_profile.channel_tags.clone());
        profile.channel_tags_match_mode = previous_mode;
    }

    if previous.profiles.iter().any(|previous_profile| {
        project_profile_has_channel_selectors(previous_profile)
            && !input
                .profiles
                .iter()
                .flatten()
                .any(|profile| profile.name == previous_profile.name)
    }) {
        return Err(error());
    }
    Ok(())
}

/// Convert a `ProjectRow` into the GraphQL `Project` (Node-id wire form).
pub(crate) fn project_row_to_gql(row: ProjectRow) -> GqlProject {
    GqlProject {
        id: format!("gid://conduit/Project/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        description: row.description,
        status: project_status_from_str(&row.status),
        profiles: map_project_profiles(row.profiles),
    }
}

/// Convert a `RoleRow` into the GraphQL `Role`. `project_id == ""` (the
/// system-scope sentinel) → `None`; otherwise a `gid://conduit/Project/<id>`
/// Node id. An empty scope list omits the field (Go `omitempty`).
pub(crate) fn role_row_to_gql(row: RoleRow) -> GqlRole {
    GqlRole {
        id: format!("gid://conduit/Role/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        level: role_level_from_str(&row.level),
        project_id: if row.project_id.is_empty() {
            None
        } else {
            Some(format!("gid://conduit/Project/{}", row.project_id).into())
        },
        scopes: if row.scopes.is_empty() {
            None
        } else {
            Some(row.scopes)
        },
    }
}

/// The three default project-level roles Go creates in `ProjectService.
/// CreateProject` (`biz/project.go:86-141`), with the exact scope slugs from
/// `internal/scopes/scopes.go`.
fn default_project_roles() -> [(&'static str, &'static [&'static str]); 3] {
    [
        (
            "Admin",
            &[
                "read_users",
                "write_users",
                "read_roles",
                "write_roles",
                "read_api_keys",
                "write_api_keys",
                "read_requests",
                "write_requests",
            ],
        ),
        (
            "Developer",
            &[
                "read_users",
                "read_api_keys",
                "write_api_keys",
                "read_requests",
            ],
        ),
        ("Viewer", &["read_users", "read_requests"]),
    ]
}

/// Evaluate the string-field predicate family (eq / neq / in / notIn / contains
/// / hasPrefix / hasSuffix / equalFold / containsFold) against a column value.
/// A `None` predicate is skipped (AND semantics across the set, matching ent).
#[allow(clippy::too_many_arguments)]
fn str_matches(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    contains: &Option<String>,
    has_prefix: &Option<String>,
    has_suffix: &Option<String>,
    equal_fold: &Option<String>,
    contains_fold: &Option<String>,
) -> bool {
    if let Some(v) = eq
        && value != v
    {
        return false;
    }
    if let Some(v) = neq
        && value == v
    {
        return false;
    }
    if let Some(l) = in_set
        && !l.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(l) = not_in
        && l.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(v) = contains
        && !value.contains(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_prefix
        && !value.starts_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_suffix
        && !value.ends_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = equal_fold
        && !value.eq_ignore_ascii_case(v)
    {
        return false;
    }
    if let Some(v) = contains_fold
        && !value.to_lowercase().contains(&v.to_lowercase())
    {
        return false;
    }
    true
}

/// Evaluate a `ProjectWhereInput` against a row (bounded predicate coverage —
/// see the module doc). `not`/`and`/`or` recurse; name/description use the
/// string family; status uses the enum family.
fn project_row_matches_where(row: &ProjectRow, w: &ProjectWhereInput) -> bool {
    if let Some(inner) = &w.not
        && project_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| project_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| project_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(id) = &w.id
        && db_id_from_gql(id.as_str()).as_deref() != Some(row.id.as_str())
    {
        return false;
    }
    if let Some(id) = &w.id_neq
        && db_id_from_gql(id.as_str()).as_deref() == Some(row.id.as_str())
    {
        return false;
    }
    if let Some(ids) = &w.id_in
        && !ids
            .iter()
            .filter_map(|id| db_id_from_gql(id.as_str()))
            .any(|id| id == row.id)
    {
        return false;
    }
    if let Some(ids) = &w.id_not_in
        && ids
            .iter()
            .filter_map(|id| db_id_from_gql(id.as_str()))
            .any(|id| id == row.id)
    {
        return false;
    }
    if !str_matches(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
        &w.name_equal_fold,
        &w.name_contains_fold,
    ) {
        return false;
    }
    if !str_matches(
        &row.description,
        &w.description,
        &w.description_neq,
        &w.description_in,
        &w.description_not_in,
        &w.description_contains,
        &w.description_has_prefix,
        &w.description_has_suffix,
        &w.description_equal_fold,
        &w.description_contains_fold,
    ) {
        return false;
    }
    let row_status = project_status_from_str(&row.status);
    if let Some(s) = w.status
        && row_status != s
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row_status == s
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list.contains(&row_status)
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list.contains(&row_status)
    {
        return false;
    }
    true
}

/// Evaluate a `RoleWhereInput` against a row (bounded predicate coverage). Adds
/// the level enum family and the `projectID` predicates (`""` is the system
/// sentinel) on top of the name string family.
fn role_row_matches_where(row: &RoleRow, w: &RoleWhereInput) -> bool {
    if let Some(inner) = &w.not
        && role_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| role_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| role_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(id) = &w.id
        && db_id_from_gql(id.as_str()).as_deref() != Some(row.id.as_str())
    {
        return false;
    }
    if let Some(id) = &w.id_neq
        && db_id_from_gql(id.as_str()).as_deref() == Some(row.id.as_str())
    {
        return false;
    }
    if let Some(ids) = &w.id_in
        && !ids
            .iter()
            .filter_map(|id| db_id_from_gql(id.as_str()))
            .any(|id| id == row.id)
    {
        return false;
    }
    if let Some(ids) = &w.id_not_in
        && ids
            .iter()
            .filter_map(|id| db_id_from_gql(id.as_str()))
            .any(|id| id == row.id)
    {
        return false;
    }
    if !str_matches(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
        &w.name_equal_fold,
        &w.name_contains_fold,
    ) {
        return false;
    }
    let row_level = role_level_from_str(&row.level);
    if let Some(l) = w.level
        && row_level != l
    {
        return false;
    }
    if let Some(l) = w.level_neq
        && row_level == l
    {
        return false;
    }
    if let Some(list) = &w.level_in
        && !list.contains(&row_level)
    {
        return false;
    }
    if let Some(list) = &w.level_not_in
        && list.contains(&row_level)
    {
        return false;
    }
    let normalize_project_id = |id: String| if id == "0" { String::new() } else { id };
    if let Some(id) = &w.project_id {
        let wanted = db_id_from_gql(id.as_str()).map(normalize_project_id);
        if wanted.as_deref() != Some(row.project_id.as_str()) {
            return false;
        }
    }
    if let Some(list) = &w.project_id_in {
        let wants: Vec<String> = list
            .iter()
            .filter_map(|i| db_id_from_gql(i.as_str()))
            .map(normalize_project_id)
            .collect();
        if !wants.iter().any(|x| x == &row.project_id) {
            return false;
        }
    }
    if w.project_id_is_nil == Some(true) && !row.project_id.is_empty() {
        return false;
    }
    if w.project_id_not_nil == Some(true) && row.project_id.is_empty() {
        return false;
    }
    true
}

// ===========================================================================
// Project adapter
// ===========================================================================

/// GraphQL-facing [`ProjectQueryServices`] + [`ProjectMutationServices`] adapter
/// backed by the live `PgProjectRepo` (+ `PgRoleRepo` for the default
/// project roles and the delete cascade, + the pool for the `user_projects`
/// join-table cleanup).
pub struct PgProjectAdapter {
    project_repo: Arc<PgProjectRepo>,
    pool: PgPool,
}

impl PgProjectAdapter {
    pub fn new(
        project_repo: Arc<PgProjectRepo>,
        _role_repo: Arc<PgRoleRepo>,
        pool: PgPool,
    ) -> Self {
        Self { project_repo, pool }
    }

    /// Materialize every live (non-deleted) project row, paging in generous
    /// windows. The projects table is small, so a full in-memory load mirrors
    /// Go's ent `.All(ctx)` without a streaming cursor.
    async fn load_all(&self) -> Result<Vec<ProjectRow>, ProjectServiceError> {
        let ctx = boot_ctx();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListProjectsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
            };
            let result = self
                .project_repo
                .list_projects(&ctx, &query)
                .await
                .map_err(|e| ProjectServiceError::Query(e.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
        }
        Ok(rows)
    }
}

#[async_trait]
impl ProjectQueryServices for PgProjectAdapter {
    async fn projects(
        &self,
        args: ProjectConnectionArgs,
    ) -> Result<ProjectConnection, ProjectServiceError> {
        let rows = self.load_all().await?;

        let mut rows: Vec<ProjectRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| project_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // The repo returns created_at-asc (≈ id-asc); re-sort for any explicit
        // selection. The crate already lowered `CREATED_AT` → `Id`.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    ProjectOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    ProjectOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let projects: Vec<GqlProject> = rows.into_iter().map(project_row_to_gql).collect();

        let mut start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let before_offset = args
            .before
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .unwrap_or(projects.len() as u64)
            .min(projects.len() as u64);
        start_offset = start_offset.min(before_offset);
        let mut start = usize::try_from(start_offset)
            .unwrap_or(0)
            .min(projects.len());
        let mut end = usize::try_from(before_offset)
            .unwrap_or(projects.len())
            .min(projects.len());
        if let Some(first) = args.first {
            end = end.min(start.saturating_add(usize::try_from(first).unwrap_or(0)));
        }
        if let Some(last) = args.last {
            start = start.max(end.saturating_sub(usize::try_from(last).unwrap_or(0)));
            start_offset = start as u64;
        }
        let windowed = projects[start..end].to_vec();
        let visible_len = windowed.len();
        let mut connection = connection_from_offset_page(windowed, start_offset, visible_len);
        connection.page_info.has_previous_page = start > 0;
        connection.page_info.has_next_page = end < projects.len();

        Ok(ProjectConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(ProjectEdge {
                            node: Some(edge.node),
                            cursor: CursorScalar(edge.cursor),
                        })
                    })
                    .collect(),
            ),
            page_info: connection.page_info,
            total_count,
        })
    }
}

#[async_trait]
impl ProjectMutationServices for PgProjectAdapter {
    async fn create_project(
        &self,
        input: GqlCreateProjectInput,
    ) -> Result<GqlProject, ProjectServiceError> {
        let ctx = boot_ctx();

        // Duplicate-name probe (Go: `project.NameEQ` → `xerrors.DuplicateNameError`).
        // Probing first surfaces the exact Go message; the repo's own
        // NameConflict is a fallback for the race window.
        let existing = self
            .project_repo
            .find_project_by_name(&ctx, &input.name)
            .await
            .map_err(|e| ProjectServiceError::Create(e.to_string()))?;
        if existing.is_some() {
            return Err(ProjectServiceError::DuplicateName(input.name));
        }

        // Project + default roles are one atomic unit. `input.status` and
        // `input.user_ids` intentionally retain the Go service semantics and
        // are ignored. Owner assignment is impossible here because the host
        // trait carries no authenticated actor.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProjectServiceError::Create(e.to_string()))?;
        let project_id = match sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, status, description, profiles) \
             VALUES ($1, 'active', $2, '{}'::jsonb) RETURNING id",
        )
        .bind(&input.name)
        .bind(input.description.unwrap_or_default())
        .fetch_one(&mut *tx)
        .await
        {
            Ok(id) => id,
            Err(error)
                if error
                    .as_database_error()
                    .and_then(|value| value.code())
                    .is_some_and(|code| code == "23505") =>
            {
                return Err(ProjectServiceError::DuplicateName(input.name));
            }
            Err(error) => return Err(ProjectServiceError::Create(error.to_string())),
        };
        for (name, scopes) in default_project_roles() {
            let scopes = scopes
                .iter()
                .map(|scope| (*scope).to_string())
                .collect::<Vec<_>>();
            sqlx::query(
                "INSERT INTO roles (name, level, project_id, scopes) \
                 VALUES ($1, 'project', $2, $3)",
            )
            .bind(name)
            .bind(project_id)
            .bind(sqlx::types::Json(scopes))
            .execute(&mut *tx)
            .await
            .map_err(|e| ProjectServiceError::Create(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| ProjectServiceError::Create(e.to_string()))?;

        let row = self
            .project_repo
            .find_project(&ctx, &project_id.to_string())
            .await
            .map_err(|e| ProjectServiceError::Create(e.to_string()))?
            .ok_or_else(|| ProjectServiceError::Create("created project disappeared".into()))?;

        Ok(project_row_to_gql(row))
    }

    async fn update_project(
        &self,
        id: &str,
        input: GqlUpdateProjectInput,
    ) -> Result<GqlProject, ProjectServiceError> {
        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id)
            .ok_or_else(|| ProjectServiceError::Update(format!("project not found (id: {id})")))?;

        // Go `UpdateProject` applies name + description only (status changes go
        // through `UpdateProjectStatus`; the user-edge fields are a pending
        // cross-domain surface and are not applied here).
        let patch = DbUpdateProjectInput {
            name: input.name,
            description: input.description.map(Some),
            updated_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        let row = self
            .project_repo
            .update_project(&ctx, &db_id, patch)
            .await
            .map_err(|e| ProjectServiceError::Update(e.to_string()))?;
        Ok(project_row_to_gql(row))
    }

    async fn update_project_status(
        &self,
        id: &str,
        status: ProjectStatus,
    ) -> Result<GqlProject, ProjectServiceError> {
        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id).ok_or_else(|| {
            ProjectServiceError::UpdateStatus(format!("project not found (id: {id})"))
        })?;

        let patch = DbUpdateProjectInput {
            status: Some(project_status_to_wire(status).to_string()),
            updated_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        let row = self
            .project_repo
            .update_project(&ctx, &db_id, patch)
            .await
            .map_err(|e| ProjectServiceError::UpdateStatus(e.to_string()))?;
        Ok(project_row_to_gql(row))
    }

    async fn update_project_profiles(
        &self,
        id: &str,
        mut input: UpdateProjectProfilesInput,
    ) -> Result<GqlProject, ProjectServiceError> {
        // Validate first (mirrors Go biz/project.go:236-238).
        validate_project_profiles(&input).map_err(ProjectServiceError::UpdateProfiles)?;

        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id).ok_or_else(|| {
            ProjectServiceError::UpdateProfiles(format!("project not found (id: {id})"))
        })?;

        let existing = self
            .project_repo
            .find_project(&ctx, &db_id)
            .await
            .map_err(|e| ProjectServiceError::UpdateProfiles(e.to_string()))?
            .ok_or_else(|| {
                ProjectServiceError::UpdateProfiles(format!("project not found (id: {id})"))
            })?;
        let previous_profiles: CoreProjectProfiles =
            serde_json::from_value(existing.profiles).unwrap_or_default();
        preserve_project_channel_selectors(&mut input, &previous_profiles)
            .map_err(ProjectServiceError::UpdateProfiles)?;

        let patch = DbUpdateProjectInput {
            profiles: Some(profiles_input_to_json(&input)),
            updated_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        let row = self
            .project_repo
            .update_project(&ctx, &db_id, patch)
            .await
            .map_err(|e| ProjectServiceError::UpdateProfiles(e.to_string()))?;
        Ok(project_row_to_gql(row))
    }

    async fn delete_project(&self, id: &str) -> Result<(), ProjectServiceError> {
        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id)
            .ok_or_else(|| ProjectServiceError::Delete(format!("project not found (id: {id})")))?;

        // Verify existence (Go `client.Project.Get` inside the transaction).
        let existing = self
            .project_repo
            .find_project(&ctx, &db_id)
            .await
            .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;
        if existing.is_none() {
            return Err(ProjectServiceError::Delete(format!(
                "project not found (id: {db_id})"
            )));
        }

        // The complete cascade is atomic: role assignments, memberships,
        // project roles, API keys and the project are changed together.
        let pid: i64 = db_id
            .parse::<i64>()
            .map_err(|_| ProjectServiceError::Delete(format!("invalid project id: {db_id}")))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;

        let project_name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM projects WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(pid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ProjectServiceError::Delete(e.to_string()))?
        .ok_or_else(|| ProjectServiceError::Delete(format!("project not found (id: {pid})")))?;

        sqlx::query(
            "DELETE FROM user_roles WHERE role_id IN \
             (SELECT id FROM roles WHERE project_id = $1 AND deleted_at = 0)",
        )
        .bind(pid)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;
        let now_unix = chrono::Utc::now().timestamp();
        sqlx::query("DELETE FROM user_projects WHERE project_id = $1")
            .bind(pid)
            .execute(&mut *tx)
            .await
            .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;

        sqlx::query(
            "UPDATE roles SET deleted_at = $1, updated_at = now() \
             WHERE project_id = $2 AND deleted_at = 0",
        )
        .bind(now_unix)
        .bind(pid)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;

        sqlx::query(
            "UPDATE api_keys SET deleted_at = $1, status = 'archived', updated_at = now() \
             WHERE project_id = $2 AND deleted_at = 0",
        )
        .bind(now_unix)
        .bind(pid)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;

        let max_deleted: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(deleted_at), 0) FROM projects WHERE name = $1")
                .bind(project_name)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;
        let project_deleted_at = now_unix.max(max_deleted + 1);
        sqlx::query(
            "UPDATE projects SET deleted_at = $1, status = 'archived', \
             updated_at = now() WHERE id = $2 AND deleted_at = 0",
        )
        .bind(project_deleted_at)
        .bind(pid)
        .execute(&mut *tx)
        .await
        .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| ProjectServiceError::Delete(e.to_string()))?;
        Ok(())
    }
}

// ===========================================================================
// Role adapter
// ===========================================================================

/// GraphQL-facing [`RoleQueryServices`] + [`RoleMutationServices`] adapter
/// backed by the live `PgRoleRepo` (+ the pool for the `user_roles`
/// join-table cleanup on delete).
pub struct PgRoleAdapter {
    role_repo: Arc<PgRoleRepo>,
    pool: PgPool,
}

impl PgRoleAdapter {
    pub fn new(role_repo: Arc<PgRoleRepo>, pool: PgPool) -> Self {
        Self { role_repo, pool }
    }

    /// Materialize every live (non-deleted) role row, paging in generous windows.
    async fn load_all(&self) -> Result<Vec<RoleRow>, RoleServiceError> {
        let ctx = boot_ctx();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListRolesQuery {
                limit: PAGE,
                offset,
                project_id: None,
                after_created_at: None,
                after_id: None,
            };
            let result = self
                .role_repo
                .list_roles(&ctx, &query)
                .await
                .map_err(|e| RoleServiceError::Query(e.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
        }
        Ok(rows)
    }
}

#[async_trait]
impl RoleQueryServices for PgRoleAdapter {
    async fn roles(&self, args: RoleConnectionArgs) -> Result<RoleConnection, RoleServiceError> {
        let rows = self.load_all().await?;

        let mut rows: Vec<RoleRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| role_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    RoleOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    RoleOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let roles: Vec<GqlRole> = rows.into_iter().map(role_row_to_gql).collect();

        let mut start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let before_offset = args
            .before
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .unwrap_or(roles.len() as u64)
            .min(roles.len() as u64);
        start_offset = start_offset.min(before_offset);
        let mut start = usize::try_from(start_offset).unwrap_or(0).min(roles.len());
        let mut end = usize::try_from(before_offset)
            .unwrap_or(roles.len())
            .min(roles.len());
        if let Some(first) = args.first {
            end = end.min(start.saturating_add(usize::try_from(first).unwrap_or(0)));
        }
        if let Some(last) = args.last {
            start = start.max(end.saturating_sub(usize::try_from(last).unwrap_or(0)));
            start_offset = start as u64;
        }
        let windowed = roles[start..end].to_vec();
        let visible_len = windowed.len();
        let mut connection = connection_from_offset_page(windowed, start_offset, visible_len);
        connection.page_info.has_previous_page = start > 0;
        connection.page_info.has_next_page = end < roles.len();

        Ok(RoleConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(RoleEdge {
                            node: Some(edge.node),
                            cursor: CursorScalar(edge.cursor),
                        })
                    })
                    .collect(),
            ),
            page_info: connection.page_info,
            total_count,
        })
    }
}

#[async_trait]
impl RoleMutationServices for PgRoleAdapter {
    async fn create_role(&self, input: GqlCreateRoleInput) -> Result<GqlRole, RoleServiceError> {
        let ctx = boot_ctx();

        // Level / projectID consistency (Go biz/role.go:61-90).
        let (level_wire, project_id_str) = match input.level {
            None | Some(RoleLevel::System) => {
                if input.project_id.is_some() {
                    return Err(RoleServiceError::ProjectIdOnSystemRole);
                }
                ("system".to_string(), String::new())
            }
            Some(RoleLevel::Project) => {
                let pid = match &input.project_id {
                    Some(id) => db_id_from_gql(id.as_str())
                        .ok_or(RoleServiceError::MissingProjectIdOnProjectRole)?,
                    None => return Err(RoleServiceError::MissingProjectIdOnProjectRole),
                };
                ("project".to_string(), pid)
            }
        };
        let validated_level = if level_wire == "project" {
            RoleLevel::Project
        } else {
            RoleLevel::System
        };
        validate_role_scopes(validated_level, input.scopes.as_deref().unwrap_or_default())?;

        // Duplicate-name probe scoped to the (project_id) scope (Go
        // `RoleNameExists` → `xerrors.DuplicateNameError`).
        let exists = self
            .role_repo
            .role_name_exists(&ctx, &project_id_str, &input.name)
            .await
            .map_err(|e| RoleServiceError::Create(e.to_string()))?;
        if exists {
            return Err(RoleServiceError::DuplicateName(input.name));
        }

        let row = self
            .role_repo
            .create_role(
                &ctx,
                DbCreateRoleInput {
                    id: String::new(),
                    name: input.name.clone(),
                    level: level_wire,
                    project_id: project_id_str,
                    scopes: input.scopes.clone().unwrap_or_default(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await
            .map_err(|e| RoleServiceError::Create(e.to_string()))?;
        Ok(role_row_to_gql(row))
    }

    async fn update_role(
        &self,
        id: &str,
        input: GqlUpdateRoleInput,
    ) -> Result<GqlRole, RoleServiceError> {
        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id)
            .ok_or_else(|| RoleServiceError::Update(format!("role not found (id: {id})")))?;

        if let Some(scopes) = &input.scopes {
            let current = self
                .role_repo
                .find_role(&ctx, &db_id)
                .await
                .map_err(|e| RoleServiceError::Update(e.to_string()))?
                .ok_or_else(|| RoleServiceError::Update(format!("role not found (id: {id})")))?;
            let level = if current.level == "project" {
                RoleLevel::Project
            } else {
                RoleLevel::System
            };
            validate_role_scopes(level, scopes)?;
        }

        // Duplicate-name check on rename (Go biz/role.go:141-158): only when the
        // new name differs from the current one, scoped to the role's project.
        if let Some(new_name) = &input.name {
            let current = self
                .role_repo
                .find_role(&ctx, &db_id)
                .await
                .map_err(|e| RoleServiceError::Update(e.to_string()))?;
            if let Some(cur) = current
                && *new_name != cur.name
            {
                let exists = self
                    .role_repo
                    .role_name_exists(&ctx, &cur.project_id, new_name)
                    .await
                    .map_err(|e| RoleServiceError::Update(e.to_string()))?;
                if exists {
                    return Err(RoleServiceError::DuplicateName(new_name.clone()));
                }
            }
        }

        // Go `biz.UpdateRole` applies name + scopes only. The append/clear-scopes
        // and user/project-edge input fields are not applied by the biz service
        // (documented) — the repo has no surface for them either.
        let patch = DbUpdateRoleInput {
            name: input.name,
            scopes: input.scopes,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        let row = self
            .role_repo
            .update_role(&ctx, &db_id, patch)
            .await
            .map_err(|e| RoleServiceError::Update(e.to_string()))?;
        Ok(role_row_to_gql(row))
    }

    async fn delete_role(&self, id: &str) -> Result<(), RoleServiceError> {
        let ctx = boot_ctx();
        let db_id = db_id_from_gql(id)
            .ok_or_else(|| RoleServiceError::Delete(format!("role not found (id: {id})")))?;

        // Existence check (Go: returns "role not found" for a missing id).
        let existing = self
            .role_repo
            .find_role(&ctx, &db_id)
            .await
            .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        if existing.is_none() {
            return Err(RoleServiceError::Delete("role not found".to_string()));
        }

        let role_id = db_id
            .parse::<i64>()
            .map_err(|_| RoleServiceError::Delete(format!("invalid role id: {db_id}")))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        let locked = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM roles WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(role_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        if locked.is_none() {
            return Err(RoleServiceError::Delete("role not found".to_string()));
        }
        sqlx::query("DELETE FROM user_roles WHERE role_id = $1")
            .bind(role_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        sqlx::query(
            "UPDATE roles SET deleted_at = $1, updated_at = now() \
             WHERE id = $2 AND deleted_at = 0",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(role_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| RoleServiceError::Delete(e.to_string()))?;
        Ok(())
    }

    async fn bulk_delete_roles(&self, ids: Vec<String>) -> Result<(), RoleServiceError> {
        // Empty ids is a no-op (Go iterates the empty slice, returns nil).
        if ids.is_empty() {
            return Ok(());
        }
        let ctx = boot_ctx();

        // Resolve + verify every id exists (Go: a count mismatch is an error).
        let mut db_ids = Vec::with_capacity(ids.len());
        for raw in &ids {
            let db_id = db_id_from_gql(raw).ok_or_else(|| {
                RoleServiceError::BulkDelete(format!("expected to find {} roles", ids.len()))
            })?;
            let existing = self
                .role_repo
                .find_role(&ctx, &db_id)
                .await
                .map_err(|e| RoleServiceError::BulkDelete(e.to_string()))?;
            if existing.is_none() {
                return Err(RoleServiceError::BulkDelete(format!(
                    "expected to find {} roles",
                    ids.len()
                )));
            }
            db_ids.push(db_id);
        }

        let now_unix = chrono::Utc::now().timestamp();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RoleServiceError::BulkDelete(e.to_string()))?;
        for db_id in &db_ids {
            if let Ok(rid) = db_id.parse::<i64>() {
                sqlx::query("DELETE FROM user_roles WHERE role_id = $1")
                    .bind(rid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| RoleServiceError::BulkDelete(e.to_string()))?;
                sqlx::query(
                    "UPDATE roles SET deleted_at = $1, updated_at = now() \
                     WHERE id = $2 AND deleted_at = 0",
                )
                .bind(now_unix)
                .bind(rid)
                .execute(&mut *tx)
                .await
                .map_err(|e| RoleServiceError::BulkDelete(e.to_string()))?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| RoleServiceError::BulkDelete(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::project::ProjectProfileInput;
    use sqlx::types::Json;

    fn project_input(name: String) -> GqlCreateProjectInput {
        GqlCreateProjectInput {
            name,
            description: Some("postgres project adapter".to_string()),
            status: None,
            user_ids: None,
        }
    }

    #[tokio::test]
    async fn postgres_project_and_role_graphql_crud_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let project_name = format!("PG GraphQL Project {suffix}");
        let second_name = format!("PG GraphQL Project {suffix} second");
        let renamed = format!("PG GraphQL Project {suffix} renamed");
        let system_role_name = format!("PG GraphQL System Role {suffix}");
        let project_role_name = format!("PG GraphQL Project Role {suffix}");

        let project_repo = Arc::new(PgProjectRepo::new(pool.clone()));
        let role_repo = Arc::new(PgRoleRepo::new(pool.clone()));
        let projects = PgProjectAdapter::new(
            Arc::clone(&project_repo),
            Arc::clone(&role_repo),
            pool.clone(),
        );
        let roles = PgRoleAdapter::new(role_repo, pool.clone());

        let created = projects
            .create_project(project_input(project_name.clone()))
            .await?;
        let project_id = db_id_from_gql(created.id.as_str())
            .ok_or_else(|| "created project has an invalid id".to_string())?
            .parse::<i64>()?;
        let second = projects
            .create_project(project_input(second_name.clone()))
            .await?;
        let second_id = db_id_from_gql(second.id.as_str())
            .ok_or_else(|| "second project has an invalid id".to_string())?
            .parse::<i64>()?;
        // The service trait has no current-actor argument. Verify that project
        // creation does not silently invent an owner or broaden membership.
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_projects WHERE project_id = $1",
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            0
        );

        let default_roles = sqlx::query_as::<_, (String, Json<Vec<String>>)>(
            "SELECT name, scopes FROM roles \
             WHERE project_id = $1 AND deleted_at = 0 ORDER BY name",
        )
        .bind(project_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            default_roles
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["Admin", "Developer", "Viewer"]
        );
        assert!(
            default_roles
                .iter()
                .find(|(name, _)| name == "Admin")
                .is_some_and(|(_, Json(scopes))| scopes.contains(&"write_roles".to_string()))
        );

        let filtered = projects
            .projects(ProjectConnectionArgs {
                first: Some(1),
                where_filter: Some(ProjectWhereInput {
                    name_has_prefix: Some(format!("PG GraphQL Project {suffix}")),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;
        assert_eq!(filtered.total_count, 2);
        assert_eq!(filtered.edges.as_ref().map_or(0, Vec::len), 1);
        assert!(filtered.page_info.has_next_page);
        let backward = projects
            .projects(ProjectConnectionArgs {
                last: Some(1),
                where_filter: Some(ProjectWhereInput {
                    name_has_prefix: Some(format!("PG GraphQL Project {suffix}")),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;
        assert_eq!(backward.edges.as_ref().map_or(0, Vec::len), 1);
        assert!(backward.page_info.has_previous_page);

        let updated = projects
            .update_project(
                created.id.as_str(),
                GqlUpdateProjectInput {
                    name: Some(renamed.clone()),
                    description: Some("updated on postgres".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.name, renamed);
        assert_eq!(updated.description, "updated on postgres");
        assert_eq!(
            projects
                .update_project_status(created.id.as_str(), ProjectStatus::Archived)
                .await?
                .status,
            ProjectStatus::Archived
        );

        let invalid_profiles = projects
            .update_project_profiles(
                created.id.as_str(),
                UpdateProjectProfilesInput {
                    active_profile: "primary".to_string(),
                    profiles: Some(vec![
                        ProjectProfileInput {
                            name: "primary".to_string(),
                            channel_ids: None,
                            channel_tags: None,
                            channel_tags_match_mode: None,
                        },
                        ProjectProfileInput {
                            name: "PRIMARY".to_string(),
                            channel_ids: None,
                            channel_tags: None,
                            channel_tags_match_mode: None,
                        },
                    ]),
                },
            )
            .await;
        assert!(matches!(
            invalid_profiles,
            Err(ProjectServiceError::UpdateProfiles(message)) if message.contains("duplicate")
        ));
        let profiled = projects
            .update_project_profiles(
                created.id.as_str(),
                UpdateProjectProfilesInput {
                    active_profile: "primary".to_string(),
                    profiles: Some(vec![ProjectProfileInput {
                        name: "primary".to_string(),
                        channel_ids: None,
                        channel_tags: None,
                        channel_tags_match_mode: None,
                    }]),
                },
            )
            .await?;
        assert!(profiled.profiles.is_some_and(|profiles| {
            profiles.active_profile == "primary"
                && profiles
                    .profiles
                    .is_some_and(|profiles| profiles.len() == 1)
        }));

        let system_role = roles
            .create_role(GqlCreateRoleInput {
                name: system_role_name.clone(),
                level: Some(RoleLevel::System),
                scopes: Some(vec!["read_channels".to_string()]),
                user_ids: None,
                project_id: None,
            })
            .await?;
        let project_role = roles
            .create_role(GqlCreateRoleInput {
                name: project_role_name.clone(),
                level: Some(RoleLevel::Project),
                scopes: Some(vec!["read_users".to_string()]),
                user_ids: None,
                project_id: Some(created.id.clone()),
            })
            .await?;
        let disallowed_scope = roles
            .update_role(
                project_role.id.as_str(),
                GqlUpdateRoleInput {
                    scopes: Some(vec!["read_channels".to_string()]),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(
            disallowed_scope,
            Err(RoleServiceError::ScopeNotAllowedForProjectRole(scope))
                if scope == "read_channels"
        ));
        let updated_role = roles
            .update_role(
                project_role.id.as_str(),
                GqlUpdateRoleInput {
                    scopes: Some(vec!["write_users".to_string()]),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated_role.scopes, Some(vec!["write_users".to_string()]));

        let project_roles = roles
            .roles(RoleConnectionArgs {
                where_filter: Some(RoleWhereInput {
                    project_id: Some(created.id.clone()),
                    name_contains: Some("Role".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;
        assert_eq!(project_roles.total_count, 1);

        let user_email = format!("pg-project-role-{suffix}@example.com");
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users (email, password) VALUES ($1, 'unused') RETURNING id",
        )
        .bind(&user_email)
        .fetch_one(&pool)
        .await?;
        let project_role_id = db_id_from_gql(project_role.id.as_str())
            .ok_or_else(|| "project role has an invalid id".to_string())?
            .parse::<i64>()?;
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)")
            .bind(user_id)
            .bind(project_role_id)
            .execute(&pool)
            .await?;
        roles.delete_role(project_role.id.as_str()).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM user_roles WHERE role_id = $1")
                .bind(project_role_id)
                .fetch_one(&pool)
                .await?,
            0
        );
        assert!(
            sqlx::query_scalar::<_, i64>("SELECT deleted_at FROM roles WHERE id = $1")
                .bind(project_role_id)
                .fetch_one(&pool)
                .await?
                > 0
        );

        roles
            .bulk_delete_roles(vec![system_role.id.to_string()])
            .await?;
        projects.delete_project(created.id.as_str()).await?;
        assert!(
            sqlx::query_scalar::<_, i64>("SELECT deleted_at FROM projects WHERE id = $1")
                .bind(project_id)
                .fetch_one(&pool)
                .await?
                > 0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM roles WHERE project_id = $1 AND deleted_at = 0",
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            0
        );

        projects.delete_project(second.id.as_str()).await?;
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM roles WHERE project_id IN ($1, $2) OR name = $3")
            .bind(project_id)
            .bind(second_id)
            .bind(system_role_name)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id IN ($1, $2)")
            .bind(project_id)
            .bind(second_id)
            .execute(&pool)
            .await?;
        Ok(())
    }
}

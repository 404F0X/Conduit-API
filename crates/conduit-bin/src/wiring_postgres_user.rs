//! PostgreSQL implementation of the admin GraphQL user domain.
//!
//! The adapter deliberately owns the multi-table transactions instead of
//! composing repository calls after the fact.  User/role/project membership
//! edits therefore cannot leave half-applied authorization state behind.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};

use conduit_admin_graphql::channel::OrderDirection;
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::role::{
    Role, RoleConnection, RoleConnectionArgs, RoleEdge, RoleLevel, RoleOrderTerm,
};
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_admin_graphql::user::{
    AddUserToProjectInput, CreateUserInput, RemoveUserFromProjectInput, UpdateProjectUserInput,
    UpdateUserInput, User, UserConnection, UserConnectionArgs, UserEdge, UserMutationServices,
    UserOrderTerm, UserProject, UserQueryServices, UserServiceError, UserStatus, UserWhereInput,
};
use conduit_auth::encode_password_bcrypt_hex;
use conduit_db::row::{RoleRow, UserProjectRow, UserRow};
use conduit_db::{ListUsersQuery, PgUserRepo, PolicyContext, Principal, RequestContext, UserRepo};
use conduit_services::user_service::OIDC_ONLY_PLACEHOLDER;

const USER_PROJECT_COLUMNS: &str = "CAST(id AS TEXT) AS id, \
CAST(user_id AS TEXT) AS user_id, CAST(project_id AS TEXT) AS project_id, \
is_owner, scopes, created_at, updated_at";

const USER_COLUMNS: &str = "CAST(id AS TEXT) AS id, email, status, prefer_language, \
first_name, last_name, avatar, is_owner, scopes, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

const ROLE_COLUMNS: &str = "CAST(r.id AS TEXT) AS id, r.name, r.level, \
COALESCE(CAST(r.project_id AS TEXT), '') AS project_id, r.scopes, \
CASE WHEN r.deleted_at = 0 THEN 'active' ELSE 'deactivated' END AS status, \
r.created_at, r.updated_at, \
CASE WHEN r.deleted_at = 0 THEN NULL ELSE to_timestamp(r.deleted_at) END AS deleted_at";

pub struct PostgresUserServiceAdapter {
    pool: PgPool,
    user_repo: Arc<PgUserRepo>,
    bcrypt_cost: u32,
}

impl PostgresUserServiceAdapter {
    pub fn with_bcrypt_cost(pool: PgPool, bcrypt_cost: u32) -> Self {
        Self {
            user_repo: Arc::new(PgUserRepo::new(pool.clone())),
            pool,
            bcrypt_cost,
        }
    }

    async fn load_all_users(&self) -> Result<Vec<UserRow>, UserServiceError> {
        let ctx = service_ctx();
        let mut rows = Vec::new();
        let mut offset = 0;
        const PAGE: u32 = 500;
        loop {
            let result = self
                .user_repo
                .list_users(
                    &ctx,
                    &ListUsersQuery {
                        limit: PAGE,
                        offset,
                        after_created_at: None,
                        after_id: None,
                    },
                )
                .await
                .map_err(|error| UserServiceError::Query(error.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
        }
        Ok(rows)
    }

    async fn load_edge_facts(&self) -> Result<HashMap<String, UserEdgeFacts>, UserServiceError> {
        let rows = sqlx::query_as::<_, UserEdgeFactsRow>(
            "SELECT CAST(u.id AS TEXT) AS id, \
             EXISTS(SELECT 1 FROM user_projects up WHERE up.user_id = u.id) AS has_projects, \
             EXISTS(SELECT 1 FROM api_keys ak WHERE ak.user_id = u.id AND ak.deleted_at = 0) AS has_api_keys, \
             EXISTS(SELECT 1 FROM user_roles ur WHERE ur.user_id = u.id) AS has_roles, \
             EXISTS(SELECT 1 FROM channel_override_templates cot WHERE cot.user_id = u.id AND cot.deleted_at = 0) AS has_channel_override_templates, \
             EXISTS(SELECT 1 FROM oidc_identities oi WHERE oi.user_id = u.id AND oi.deleted_at = 0) AS has_oidc_identities \
             FROM users u WHERE u.deleted_at = 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| UserServiceError::Query(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.id,
                    UserEdgeFacts {
                        has_projects: row.has_projects,
                        has_api_keys: row.has_api_keys,
                        has_roles: row.has_roles,
                        has_channel_override_templates: row.has_channel_override_templates,
                        has_oidc_identities: row.has_oidc_identities,
                    },
                )
            })
            .collect())
    }
}

fn service_ctx() -> RequestContext {
    // GraphQL's admin schema already authenticated and authorized this call.
    // Use the explicit service principal for the repository boundary; unlike
    // the old Node resolver this never manufactures a caller/test principal.
    RequestContext::new(PolicyContext::new(Principal::system()))
}

fn db_id(raw: &str) -> Result<i64, UserServiceError> {
    if let Ok(guid) = parse_guid(raw) {
        return Ok(guid.id);
    }
    raw.parse::<i64>()
        .map_err(|_| UserServiceError::NotFound(raw.to_string()))
}

fn status_to_wire(status: UserStatus) -> &'static str {
    match status {
        UserStatus::Activated => "activated",
        UserStatus::Deactivated => "deactivated",
    }
}

fn status_from_wire(status: &str) -> UserStatus {
    if status == "deactivated" {
        UserStatus::Deactivated
    } else {
        UserStatus::Activated
    }
}

pub(crate) fn user_to_gql(row: UserRow) -> User {
    User {
        id: format!("gid://conduit/User/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        email: row.email,
        status: status_from_wire(&row.status),
        prefer_language: row.prefer_language,
        first_name: row.first_name,
        last_name: row.last_name,
        avatar: row.avatar.filter(|value| !value.is_empty()),
        is_owner: row.is_owner,
        scopes: Some(row.scopes),
    }
}

pub(crate) fn user_project_to_gql(row: UserProjectRow) -> UserProject {
    UserProject {
        id: format!("gid://conduit/UserProject/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        user_id: format!("gid://conduit/User/{}", row.user_id).into(),
        project_id: format!("gid://conduit/Project/{}", row.project_id).into(),
        is_owner: row.is_owner,
        scopes: Some(row.scopes),
    }
}

pub(crate) fn user_role_to_gql(
    row: conduit_db::UserRoleRow,
) -> conduit_admin_graphql::user::UserRole {
    conduit_admin_graphql::user::UserRole {
        id: format!("gid://conduit/UserRole/{}", row.id).into(),
        user_id: format!("gid://conduit/User/{}", row.user_id).into(),
        role_id: format!("gid://conduit/Role/{}", row.role_id).into(),
        created_at: row.created_at.map(TimeScalar),
        updated_at: row.updated_at.map(TimeScalar),
    }
}

fn role_to_gql(row: RoleRow) -> Role {
    Role {
        id: format!("gid://conduit/Role/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        level: if row.level == "project" {
            RoleLevel::Project
        } else {
            RoleLevel::System
        },
        project_id: if row.project_id.is_empty() {
            None
        } else {
            Some(format!("gid://conduit/Project/{}", row.project_id).into())
        },
        scopes: (!row.scopes.is_empty()).then_some(row.scopes),
    }
}

#[derive(sqlx::FromRow)]
struct UserEdgeFactsRow {
    id: String,
    has_projects: bool,
    has_api_keys: bool,
    has_roles: bool,
    has_channel_override_templates: bool,
    has_oidc_identities: bool,
}

#[derive(Debug, Default)]
struct UserEdgeFacts {
    has_projects: bool,
    has_api_keys: bool,
    has_roles: bool,
    has_channel_override_templates: bool,
    has_oidc_identities: bool,
}

fn optional_bool_matches(expected: Option<bool>, actual: bool) -> bool {
    expected.is_none_or(|expected| expected == actual)
}

#[allow(clippy::too_many_arguments)]
fn string_matches(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    gt: &Option<String>,
    gte: &Option<String>,
    lt: &Option<String>,
    lte: &Option<String>,
    contains: &Option<String>,
    prefix: &Option<String>,
    suffix: &Option<String>,
    equal_fold: &Option<String>,
    contains_fold: &Option<String>,
) -> bool {
    if eq.as_ref().is_some_and(|v| value != v)
        || neq.as_ref().is_some_and(|v| value == v)
        || in_set
            .as_ref()
            .is_some_and(|v| !v.iter().any(|x| x == value))
        || not_in
            .as_ref()
            .is_some_and(|v| v.iter().any(|x| x == value))
        || gt.as_ref().is_some_and(|v| value <= v.as_str())
        || gte.as_ref().is_some_and(|v| value < v.as_str())
        || lt.as_ref().is_some_and(|v| value >= v.as_str())
        || lte.as_ref().is_some_and(|v| value > v.as_str())
        || contains.as_ref().is_some_and(|v| !value.contains(v))
        || prefix.as_ref().is_some_and(|v| !value.starts_with(v))
        || suffix.as_ref().is_some_and(|v| !value.ends_with(v))
        || equal_fold
            .as_ref()
            .is_some_and(|v| !value.eq_ignore_ascii_case(v))
        || contains_fold
            .as_ref()
            .is_some_and(|v| !value.to_lowercase().contains(v.to_lowercase().as_str()))
    {
        return false;
    }
    true
}

fn id_predicates_match(row: &UserRow, where_: &UserWhereInput) -> bool {
    let parsed = |id: &async_graphql::ID| db_id(id.as_str()).ok();
    let row_id = row.id.parse::<i64>().unwrap_or(i64::MAX);
    !where_
        .id
        .as_ref()
        .is_some_and(|id| parsed(id) != Some(row_id))
        && !where_
            .id_neq
            .as_ref()
            .is_some_and(|id| parsed(id) == Some(row_id))
        && !where_
            .id_in
            .as_ref()
            .is_some_and(|ids| !ids.iter().any(|id| parsed(id) == Some(row_id)))
        && !where_
            .id_not_in
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| parsed(id) == Some(row_id)))
        && !where_
            .id_gt
            .as_ref()
            .is_some_and(|id| parsed(id).is_some_and(|value| row_id <= value))
        && !where_
            .id_gte
            .as_ref()
            .is_some_and(|id| parsed(id).is_some_and(|value| row_id < value))
        && !where_
            .id_lt
            .as_ref()
            .is_some_and(|id| parsed(id).is_some_and(|value| row_id >= value))
        && !where_
            .id_lte
            .as_ref()
            .is_some_and(|id| parsed(id).is_some_and(|value| row_id > value))
}

fn time_matches(
    value: &chrono::DateTime<chrono::Utc>,
    eq: &Option<TimeScalar>,
    neq: &Option<TimeScalar>,
    in_set: &Option<Vec<TimeScalar>>,
    not_in: &Option<Vec<TimeScalar>>,
    gt: &Option<TimeScalar>,
    gte: &Option<TimeScalar>,
    lt: &Option<TimeScalar>,
    lte: &Option<TimeScalar>,
) -> bool {
    !eq.as_ref().is_some_and(|v| value != &v.0)
        && !neq.as_ref().is_some_and(|v| value == &v.0)
        && !in_set
            .as_ref()
            .is_some_and(|values| !values.iter().any(|v| value == &v.0))
        && !not_in
            .as_ref()
            .is_some_and(|values| values.iter().any(|v| value == &v.0))
        && !gt.as_ref().is_some_and(|v| value <= &v.0)
        && !gte.as_ref().is_some_and(|v| value < &v.0)
        && !lt.as_ref().is_some_and(|v| value >= &v.0)
        && !lte.as_ref().is_some_and(|v| value > &v.0)
}

fn user_matches(row: &UserRow, where_: &UserWhereInput, facts: &UserEdgeFacts) -> bool {
    if where_
        .not
        .as_ref()
        .is_some_and(|inner| user_matches(row, inner, facts))
        || where_
            .and
            .as_ref()
            .is_some_and(|items| !items.iter().all(|item| user_matches(row, item, facts)))
        || where_.or.as_ref().is_some_and(|items| {
            !items.is_empty() && !items.iter().any(|item| user_matches(row, item, facts))
        })
    {
        return false;
    }
    if !id_predicates_match(row, where_)
        || !time_matches(
            &row.created_at,
            &where_.created_at,
            &where_.created_at_neq,
            &where_.created_at_in,
            &where_.created_at_not_in,
            &where_.created_at_gt,
            &where_.created_at_gte,
            &where_.created_at_lt,
            &where_.created_at_lte,
        )
        || !time_matches(
            &row.updated_at,
            &where_.updated_at,
            &where_.updated_at_neq,
            &where_.updated_at_in,
            &where_.updated_at_not_in,
            &where_.updated_at_gt,
            &where_.updated_at_gte,
            &where_.updated_at_lt,
            &where_.updated_at_lte,
        )
        || !string_matches(
            &row.email,
            &where_.email,
            &where_.email_neq,
            &where_.email_in,
            &where_.email_not_in,
            &where_.email_gt,
            &where_.email_gte,
            &where_.email_lt,
            &where_.email_lte,
            &where_.email_contains,
            &where_.email_has_prefix,
            &where_.email_has_suffix,
            &where_.email_equal_fold,
            &where_.email_contains_fold,
        )
        || !string_matches(
            &row.prefer_language,
            &where_.prefer_language,
            &where_.prefer_language_neq,
            &where_.prefer_language_in,
            &where_.prefer_language_not_in,
            &where_.prefer_language_gt,
            &where_.prefer_language_gte,
            &where_.prefer_language_lt,
            &where_.prefer_language_lte,
            &where_.prefer_language_contains,
            &where_.prefer_language_has_prefix,
            &where_.prefer_language_has_suffix,
            &where_.prefer_language_equal_fold,
            &where_.prefer_language_contains_fold,
        )
        || !string_matches(
            &row.first_name,
            &where_.first_name,
            &where_.first_name_neq,
            &where_.first_name_in,
            &where_.first_name_not_in,
            &where_.first_name_gt,
            &where_.first_name_gte,
            &where_.first_name_lt,
            &where_.first_name_lte,
            &where_.first_name_contains,
            &where_.first_name_has_prefix,
            &where_.first_name_has_suffix,
            &where_.first_name_equal_fold,
            &where_.first_name_contains_fold,
        )
        || !string_matches(
            &row.last_name,
            &where_.last_name,
            &where_.last_name_neq,
            &where_.last_name_in,
            &where_.last_name_not_in,
            &where_.last_name_gt,
            &where_.last_name_gte,
            &where_.last_name_lt,
            &where_.last_name_lte,
            &where_.last_name_contains,
            &where_.last_name_has_prefix,
            &where_.last_name_has_suffix,
            &where_.last_name_equal_fold,
            &where_.last_name_contains_fold,
        )
    {
        return false;
    }

    let avatar = row.avatar.as_deref().unwrap_or_default();
    if where_
        .avatar_is_nil
        .is_some_and(|expect_nil| row.avatar.is_none() != expect_nil)
        || where_
            .avatar_not_nil
            .is_some_and(|expect_not_nil| row.avatar.is_some() != expect_not_nil)
        || !string_matches(
            avatar,
            &where_.avatar,
            &where_.avatar_neq,
            &where_.avatar_in,
            &where_.avatar_not_in,
            &where_.avatar_gt,
            &where_.avatar_gte,
            &where_.avatar_lt,
            &where_.avatar_lte,
            &where_.avatar_contains,
            &where_.avatar_has_prefix,
            &where_.avatar_has_suffix,
            &where_.avatar_equal_fold,
            &where_.avatar_contains_fold,
        )
    {
        return false;
    }

    let status = status_from_wire(&row.status);
    if where_.status.is_some_and(|value| value != status)
        || where_.status_neq.is_some_and(|value| value == status)
        || where_
            .status_in
            .as_ref()
            .is_some_and(|values| !values.contains(&status))
        || where_
            .status_not_in
            .as_ref()
            .is_some_and(|values| values.contains(&status))
        || where_.is_owner.is_some_and(|value| value != row.is_owner)
        || where_
            .is_owner_neq
            .is_some_and(|value| value == row.is_owner)
    {
        return false;
    }

    optional_bool_matches(where_.has_projects, facts.has_projects)
        && optional_bool_matches(where_.has_project_users, facts.has_projects)
        && optional_bool_matches(where_.has_api_keys, facts.has_api_keys)
        && optional_bool_matches(where_.has_roles, facts.has_roles)
        && optional_bool_matches(where_.has_user_roles, facts.has_roles)
        && optional_bool_matches(
            where_.has_channel_override_templates,
            facts.has_channel_override_templates,
        )
        && optional_bool_matches(where_.has_oidc_identities, facts.has_oidc_identities)
}

async fn add_roles(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    role_ids: &[async_graphql::ID],
    project_constraint: Option<i64>,
    wrap: fn(String) -> UserServiceError,
) -> Result<(), UserServiceError> {
    for raw in role_ids {
        let role_id = db_id(raw.as_str())?;
        let role: Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT level, project_id FROM roles WHERE id = $1 AND deleted_at = 0")
                .bind(role_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|error| wrap(error.to_string()))?;
        let Some((level, role_project_id)) = role else {
            return Err(wrap(format!("role {role_id} not found")));
        };
        if let Some(project_id) = project_constraint
            && (level != "project" || role_project_id != Some(project_id))
        {
            return Err(wrap(format!(
                "role {role_id} does not belong to project {project_id}"
            )));
        }
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now()) \
             ON CONFLICT (user_id, role_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(role_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| wrap(error.to_string()))?;
    }
    Ok(())
}

async fn remove_roles(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    role_ids: &[async_graphql::ID],
    wrap: fn(String) -> UserServiceError,
) -> Result<(), UserServiceError> {
    for raw in role_ids {
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
            .bind(user_id)
            .bind(db_id(raw.as_str())?)
            .execute(&mut **tx)
            .await
            .map_err(|error| wrap(error.to_string()))?;
    }
    Ok(())
}

async fn ensure_not_last_project_owner(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    project_id: i64,
    is_current_owner: bool,
    wrap: fn(String) -> UserServiceError,
) -> Result<(), UserServiceError> {
    if !is_current_owner {
        return Ok(());
    }
    // Lock every owner edge, not only the target edge.  Two concurrent
    // demotions/removals must not both observe the other as an owner.
    let owners: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM user_projects \
         WHERE project_id = $1 AND is_owner = TRUE ORDER BY user_id FOR UPDATE",
    )
    .bind(project_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| wrap(error.to_string()))?;
    if !owners.into_iter().any(|owner_id| owner_id != user_id) {
        return Err(wrap(format!(
            "cannot remove the last owner of project {project_id}"
        )));
    }
    Ok(())
}

async fn ensure_not_last_system_owner(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    is_current_owner: bool,
    wrap: fn(String) -> UserServiceError,
) -> Result<(), UserServiceError> {
    if !is_current_owner {
        return Ok(());
    }
    let owners: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM users \
         WHERE is_owner = TRUE AND status = 'activated' AND deleted_at = 0 \
         ORDER BY id FOR UPDATE",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| wrap(error.to_string()))?;
    if !owners.into_iter().any(|owner_id| owner_id != user_id) {
        return Err(wrap(
            "cannot deactivate or demote the last system owner".to_string(),
        ));
    }
    Ok(())
}

#[async_trait]
impl UserQueryServices for PostgresUserServiceAdapter {
    async fn users(&self, args: UserConnectionArgs) -> Result<UserConnection, UserServiceError> {
        let facts = self.load_edge_facts().await?;
        let mut rows = self.load_all_users().await?;
        if let Some(where_) = &args.where_filter {
            let empty = UserEdgeFacts::default();
            rows.retain(|row| user_matches(row, where_, facts.get(&row.id).unwrap_or(&empty)));
        }
        if let Some(order) = args.order_by {
            rows.sort_by(|left, right| {
                let value = match order.term {
                    UserOrderTerm::Id => left
                        .id
                        .parse::<i64>()
                        .unwrap_or(i64::MAX)
                        .cmp(&right.id.parse::<i64>().unwrap_or(i64::MAX)),
                    UserOrderTerm::UpdatedAt => left.updated_at.cmp(&right.updated_at),
                };
                match order.direction {
                    OrderDirection::Asc => value,
                    OrderDirection::Desc => value.reverse(),
                }
            });
        }
        let total_count = rows.len() as i64;
        let users: Vec<User> = rows.into_iter().map(user_to_gql).collect();
        let after = args
            .after
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .map_or(0, |offset| offset.saturating_add(1));
        let before = args
            .before
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .unwrap_or(users.len() as u64)
            .min(users.len() as u64);
        let mut page_start = after.min(before) as usize;
        let mut page_end = before as usize;
        if let Some(first) = args.first.and_then(|value| usize::try_from(value).ok()) {
            page_end = page_end.min(page_start.saturating_add(first));
        }
        if let Some(last) = args.last.and_then(|value| usize::try_from(value).ok())
            && page_end.saturating_sub(page_start) > last
        {
            page_start = page_end - last;
        }
        let selected = users[page_start..page_end].to_vec();
        let page_size = selected.len();
        let mut connection = connection_from_offset_page(selected, page_start as u64, page_size);
        connection.page_info.has_previous_page = page_start > 0;
        connection.page_info.has_next_page = page_end < users.len();
        Ok(UserConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(UserEdge {
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

    async fn roles_for_user(
        &self,
        user_id: &str,
        args: RoleConnectionArgs,
    ) -> Result<RoleConnection, UserServiceError> {
        let user_id = db_id(user_id)?;
        let mut rows = sqlx::query_as::<_, RoleRow>(&format!(
            "SELECT {ROLE_COLUMNS} FROM roles r \
             JOIN user_roles ur ON ur.role_id = r.id \
             WHERE ur.user_id = $1 AND r.deleted_at = 0"
        ))
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| UserServiceError::Query(error.to_string()))?;
        if let Some(project_id) = args
            .where_filter
            .as_ref()
            .and_then(|filter| filter.project_id.as_ref())
        {
            let project_id = db_id(project_id.as_str())?.to_string();
            rows.retain(|row| row.project_id == project_id);
        }
        rows.sort_by(|left, right| {
            let value = match args.order_by.as_ref().map(|order| order.term) {
                Some(RoleOrderTerm::UpdatedAt) => left.updated_at.cmp(&right.updated_at),
                _ => left
                    .id
                    .parse::<i64>()
                    .unwrap_or(i64::MAX)
                    .cmp(&right.id.parse::<i64>().unwrap_or(i64::MAX)),
            };
            if args
                .order_by
                .as_ref()
                .is_some_and(|order| order.direction == OrderDirection::Desc)
            {
                value.reverse()
            } else {
                value
            }
        });
        let total_count = rows.len() as i64;
        let roles: Vec<Role> = rows.into_iter().map(role_to_gql).collect();
        let after = args
            .after
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .map_or(0, |offset| offset.saturating_add(1));
        let before = args
            .before
            .as_deref()
            .and_then(|cursor| decode_offset_cursor(cursor).ok())
            .unwrap_or(roles.len() as u64)
            .min(roles.len() as u64);
        let mut page_start = after.min(before) as usize;
        let mut page_end = before as usize;
        if let Some(first) = args.first.and_then(|value| usize::try_from(value).ok()) {
            page_end = page_end.min(page_start.saturating_add(first));
        }
        if let Some(last) = args.last.and_then(|value| usize::try_from(value).ok())
            && page_end.saturating_sub(page_start) > last
        {
            page_start = page_end - last;
        }
        let selected = roles[page_start..page_end].to_vec();
        let page_size = selected.len();
        let mut connection = connection_from_offset_page(selected, page_start as u64, page_size);
        connection.page_info.has_previous_page = page_start > 0;
        connection.page_info.has_next_page = page_end < roles.len();
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

    async fn project_users(&self, project_id: &str) -> Result<Vec<UserProject>, UserServiceError> {
        let rows = sqlx::query_as::<_, UserProjectRow>(&format!(
            "SELECT {USER_PROJECT_COLUMNS} FROM user_projects \
             WHERE project_id = $1 ORDER BY id"
        ))
        .bind(db_id(project_id)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| UserServiceError::Query(error.to_string()))?;
        Ok(rows.into_iter().map(user_project_to_gql).collect())
    }
}

fn unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
}

#[async_trait]
impl UserMutationServices for PostgresUserServiceAdapter {
    async fn create_user(&self, input: CreateUserInput) -> Result<User, UserServiceError> {
        let password = if input.password == OIDC_ONLY_PLACEHOLDER {
            OIDC_ONLY_PLACEHOLDER.to_string()
        } else {
            encode_password_bcrypt_hex(&input.password, self.bcrypt_cost)
                .map_err(|error| UserServiceError::Create(error.to_string()))?
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::Create(error.to_string()))?;
        let result = sqlx::query_as::<_, UserRow>(&format!(
            "INSERT INTO users (email, password, first_name, last_name, scopes) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {USER_COLUMNS}"
        ))
        .bind(&input.email)
        .bind(password)
        .bind(input.first_name.unwrap_or_default())
        .bind(input.last_name.unwrap_or_default())
        .bind(sqlx::types::Json(input.scopes.unwrap_or_default()))
        .fetch_one(&mut *tx)
        .await;
        let row = match result {
            Ok(row) => row,
            Err(error) if unique_violation(&error) => {
                return Err(UserServiceError::DuplicateEmail(input.email));
            }
            Err(error) => return Err(UserServiceError::Create(error.to_string())),
        };
        if let Some(role_ids) = &input.role_ids {
            add_roles(
                &mut tx,
                db_id(&row.id)?,
                role_ids,
                None,
                UserServiceError::Create,
            )
            .await?;
        }
        tx.commit()
            .await
            .map_err(|error| UserServiceError::Create(error.to_string()))?;
        Ok(user_to_gql(row))
    }

    async fn update_user(
        &self,
        id: &str,
        input: UpdateUserInput,
    ) -> Result<User, UserServiceError> {
        let user_id = db_id(id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::Update(error.to_string()))?;
        let current: Option<(sqlx::types::Json<Vec<String>>, bool, String)> = sqlx::query_as(
            "SELECT scopes, is_owner, status FROM users \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| UserServiceError::Update(error.to_string()))?;
        let Some((current_scopes, current_owner, current_status)) = current else {
            return Err(UserServiceError::Update("user not found".to_string()));
        };
        if input.is_owner == Some(false) && current_status == "activated" {
            ensure_not_last_system_owner(&mut tx, user_id, current_owner, UserServiceError::Update)
                .await?;
        }
        let scopes = if input.clear_scopes == Some(true) {
            Some(Vec::new())
        } else if input.scopes.is_some() || input.append_scopes.is_some() {
            let mut scopes = input.scopes.clone().unwrap_or(current_scopes.0);
            scopes.extend(input.append_scopes.clone().unwrap_or_default());
            Some(scopes)
        } else {
            None
        };
        let avatar = if input.clear_avatar == Some(true) {
            Some(None)
        } else {
            input.avatar.clone().map(Some)
        };
        let password = match input.password.as_ref() {
            Some(password) => Some(
                encode_password_bcrypt_hex(password, self.bcrypt_cost)
                    .map_err(|error| UserServiceError::Update(error.to_string()))?,
            ),
            None => None,
        };
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE users SET ");
        let mut set = builder.separated(", ");
        if let Some(value) = input.email.clone() {
            set.push("email = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.prefer_language.clone() {
            set.push("prefer_language = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.first_name.clone() {
            set.push("first_name = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.last_name.clone() {
            set.push("last_name = ").push_bind_unseparated(value);
        }
        if let Some(value) = avatar {
            set.push("avatar = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.is_owner {
            set.push("is_owner = ").push_bind_unseparated(value);
        }
        if let Some(value) = scopes {
            set.push("scopes = ")
                .push_bind_unseparated(sqlx::types::Json(value));
        }
        if let Some(value) = password {
            set.push("password = ").push_bind_unseparated(value);
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(user_id)
            .push(" AND deleted_at = 0");
        if let Err(error) = builder.build().execute(&mut *tx).await {
            if unique_violation(&error) {
                return Err(UserServiceError::DuplicateEmail(
                    input.email.unwrap_or_default(),
                ));
            }
            return Err(UserServiceError::Update(error.to_string()));
        }
        if input.clear_roles == Some(true) {
            sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|error| UserServiceError::Update(error.to_string()))?;
        }
        if let Some(add) = &input.add_role_ids {
            add_roles(&mut tx, user_id, add, None, UserServiceError::Update).await?;
        }
        if let Some(remove) = &input.remove_role_ids {
            remove_roles(&mut tx, user_id, remove, UserServiceError::Update).await?;
        }
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| UserServiceError::Update(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| UserServiceError::Update(error.to_string()))?;
        Ok(user_to_gql(row))
    }

    async fn update_user_status(
        &self,
        id: &str,
        status: UserStatus,
    ) -> Result<User, UserServiceError> {
        let user_id = db_id(id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::UpdateStatus(error.to_string()))?;
        let current: Option<(bool, String)> = sqlx::query_as(
            "SELECT is_owner, status FROM users \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| UserServiceError::UpdateStatus(error.to_string()))?;
        let Some((is_owner, current_status)) = current else {
            return Err(UserServiceError::UpdateStatus("user not found".to_string()));
        };
        if status == UserStatus::Deactivated && current_status == "activated" {
            ensure_not_last_system_owner(
                &mut tx,
                user_id,
                is_owner,
                UserServiceError::UpdateStatus,
            )
            .await?;
        }
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "UPDATE users SET status = $1, updated_at = now() \
             WHERE id = $2 AND deleted_at = 0 RETURNING {USER_COLUMNS}"
        ))
        .bind(status_to_wire(status))
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| UserServiceError::UpdateStatus(error.to_string()))?
        .ok_or_else(|| UserServiceError::UpdateStatus("user not found".to_string()))?;
        tx.commit()
            .await
            .map_err(|error| UserServiceError::UpdateStatus(error.to_string()))?;
        Ok(user_to_gql(row))
    }

    async fn delete_user(&self, id: &str) -> Result<(), UserServiceError> {
        let user_id = db_id(id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        let user: Option<(String, bool)> = sqlx::query_as(
            "SELECT email, is_owner FROM users \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        let Some((email, is_owner)) = user else {
            return Err(UserServiceError::Delete("failed to get user".to_string()));
        };
        if is_owner {
            return Err(UserServiceError::Delete(
                "cannot delete owner user, transfer ownership first".to_string(),
            ));
        }
        let memberships: Vec<(i64, bool)> = sqlx::query_as(
            "SELECT project_id, is_owner FROM user_projects \
             WHERE user_id = $1 ORDER BY project_id",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        for (project_id, membership_owner) in memberships {
            ensure_not_last_project_owner(
                &mut tx,
                user_id,
                project_id,
                membership_owner,
                UserServiceError::Delete,
            )
            .await?;
        }
        sqlx::query("DELETE FROM user_projects WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        let max_deleted: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(deleted_at), 0) FROM users WHERE email = $1")
                .bind(email)
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        let deleted_at = chrono::Utc::now()
            .timestamp()
            .max(max_deleted.saturating_add(1));
        sqlx::query(
            "UPDATE users SET deleted_at = $1, status = 'deactivated', updated_at = now() \
             WHERE id = $2 AND deleted_at = 0",
        )
        .bind(deleted_at)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| UserServiceError::Delete(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| UserServiceError::Delete(error.to_string()))
    }

    async fn add_user_to_project(
        &self,
        input: AddUserToProjectInput,
    ) -> Result<UserProject, UserServiceError> {
        let user_id = db_id(input.user_id.as_str())?;
        let project_id = db_id(input.project_id.as_str())?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::AddToProject(error.to_string()))?;
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at = 0) \
             AND EXISTS(SELECT 1 FROM projects WHERE id = $2 AND deleted_at = 0)",
        )
        .bind(user_id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| UserServiceError::AddToProject(error.to_string()))?;
        if !valid {
            return Err(UserServiceError::AddToProject(
                "user or project not found".to_string(),
            ));
        }
        let result = sqlx::query_as::<_, UserProjectRow>(&format!(
            "INSERT INTO user_projects (user_id, project_id, is_owner, scopes) \
             VALUES ($1, $2, $3, $4) RETURNING {USER_PROJECT_COLUMNS}"
        ))
        .bind(user_id)
        .bind(project_id)
        .bind(input.is_owner.unwrap_or(false))
        .bind(sqlx::types::Json(input.scopes.unwrap_or_default()))
        .fetch_one(&mut *tx)
        .await;
        let row = match result {
            Ok(row) => row,
            Err(error) if unique_violation(&error) => {
                return Err(UserServiceError::AddToProject(format!(
                    "user {user_id} is already a member of project {project_id}"
                )));
            }
            Err(error) => return Err(UserServiceError::AddToProject(error.to_string())),
        };
        if let Some(role_ids) = &input.role_ids {
            add_roles(
                &mut tx,
                user_id,
                role_ids,
                Some(project_id),
                UserServiceError::AddToProject,
            )
            .await?;
        }
        tx.commit()
            .await
            .map_err(|error| UserServiceError::AddToProject(error.to_string()))?;
        Ok(user_project_to_gql(row))
    }

    async fn remove_user_from_project(
        &self,
        input: RemoveUserFromProjectInput,
    ) -> Result<(), UserServiceError> {
        let user_id = db_id(input.user_id.as_str())?;
        let project_id = db_id(input.project_id.as_str())?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))?;
        let memberships: Vec<(i64, bool)> = sqlx::query_as(
            "SELECT user_id, is_owner FROM user_projects \
             WHERE project_id = $1 ORDER BY user_id FOR UPDATE",
        )
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))?;
        let Some(is_owner) = memberships
            .iter()
            .find_map(|(member_id, is_owner)| (*member_id == user_id).then_some(*is_owner))
        else {
            tx.commit()
                .await
                .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))?;
            return Ok(());
        };
        ensure_not_last_project_owner(
            &mut tx,
            user_id,
            project_id,
            is_owner,
            UserServiceError::RemoveFromProject,
        )
        .await?;
        sqlx::query("DELETE FROM user_projects WHERE user_id = $1 AND project_id = $2")
            .bind(user_id)
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))?;
        sqlx::query(
            "DELETE FROM user_roles WHERE user_id = $1 \
             AND role_id IN (SELECT id FROM roles WHERE project_id = $2)",
        )
        .bind(user_id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| UserServiceError::RemoveFromProject(error.to_string()))
    }

    async fn update_project_user(
        &self,
        input: UpdateProjectUserInput,
    ) -> Result<UserProject, UserServiceError> {
        let user_id = db_id(input.user_id.as_str())?;
        let project_id = db_id(input.project_id.as_str())?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| UserServiceError::UpdateProjectUser(error.to_string()))?;
        let memberships: Vec<(i64, bool)> = sqlx::query_as(
            "SELECT user_id, is_owner FROM user_projects \
             WHERE project_id = $1 ORDER BY user_id FOR UPDATE",
        )
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| UserServiceError::UpdateProjectUser(error.to_string()))?;
        let Some(current_owner) = memberships
            .iter()
            .find_map(|(member_id, is_owner)| (*member_id == user_id).then_some(*is_owner))
        else {
            return Err(UserServiceError::UpdateProjectUser(
                "failed to find user project relationship".to_string(),
            ));
        };
        if current_owner && input.is_owner == Some(false) {
            ensure_not_last_project_owner(
                &mut tx,
                user_id,
                project_id,
                true,
                UserServiceError::UpdateProjectUser,
            )
            .await?;
        }
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE user_projects SET ");
        let mut set = builder.separated(", ");
        if let Some(value) = input.is_owner {
            set.push("is_owner = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.scopes.clone() {
            set.push("scopes = ")
                .push_bind_unseparated(sqlx::types::Json(value));
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE user_id = ")
            .push_bind(user_id)
            .push(" AND project_id = ")
            .push_bind(project_id);
        builder
            .build()
            .execute(&mut *tx)
            .await
            .map_err(|error| UserServiceError::UpdateProjectUser(error.to_string()))?;
        if let Some(add) = &input.add_role_ids {
            add_roles(
                &mut tx,
                user_id,
                add,
                Some(project_id),
                UserServiceError::UpdateProjectUser,
            )
            .await?;
        }
        if let Some(remove) = &input.remove_role_ids {
            remove_roles(
                &mut tx,
                user_id,
                remove,
                UserServiceError::UpdateProjectUser,
            )
            .await?;
        }
        let row = sqlx::query_as::<_, UserProjectRow>(&format!(
            "SELECT {USER_PROJECT_COLUMNS} FROM user_projects \
             WHERE user_id = $1 AND project_id = $2"
        ))
        .bind(user_id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| UserServiceError::UpdateProjectUser(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| UserServiceError::UpdateProjectUser(error.to_string()))?;
        Ok(user_project_to_gql(row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestError = Box<dyn std::error::Error>;

    fn create_input(email: String) -> CreateUserInput {
        CreateUserInput {
            email,
            status: None,
            prefer_language: None,
            password: "12345678".to_string(),
            first_name: Some("Postgres".to_string()),
            last_name: Some("Admin".to_string()),
            avatar: None,
            is_owner: None,
            scopes: Some(vec!["read_projects".to_string()]),
            project_ids: None,
            role_ids: None,
        }
    }

    #[tokio::test]
    async fn live_postgres_admin_user_crud_membership_roles_and_owner_guard()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        );
        let project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects (name, description) VALUES ($1, '') RETURNING id",
        )
        .bind(format!("pg-user-project-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let role_id: i64 = sqlx::query_scalar(
            "INSERT INTO roles (name, level, project_id, scopes) \
             VALUES ($1, 'project', $2, '[\"read_channels\"]'::jsonb) RETURNING id",
        )
        .bind(format!("pg-user-role-{suffix}"))
        .bind(project_id)
        .fetch_one(&pool)
        .await?;
        let adapter = Arc::new(PostgresUserServiceAdapter::with_bcrypt_cost(
            pool.clone(),
            4,
        ));
        let first = adapter
            .create_user(create_input(format!("pg-first-{suffix}@example.test")))
            .await?;
        let second = adapter
            .create_user(create_input(format!("pg-second-{suffix}@example.test")))
            .await?;
        let first_id = db_id(first.id.as_str())?;
        let second_id = db_id(second.id.as_str())?;

        let membership = adapter
            .add_user_to_project(AddUserToProjectInput {
                project_id: project_id.to_string().into(),
                user_id: first_id.to_string().into(),
                is_owner: Some(true),
                scopes: Some(vec!["read_channels".to_string()]),
                role_ids: Some(vec![role_id.to_string().into()]),
            })
            .await?;
        assert!(membership.is_owner);
        assert_eq!(
            adapter.project_users(&project_id.to_string()).await?.len(),
            1
        );
        assert_eq!(
            adapter
                .roles_for_user(first.id.as_str(), RoleConnectionArgs::default())
                .await?
                .total_count,
            1
        );
        assert!(
            adapter
                .update_project_user(UpdateProjectUserInput {
                    project_id: project_id.to_string().into(),
                    user_id: first_id.to_string().into(),
                    is_owner: Some(false),
                    scopes: None,
                    add_role_ids: None,
                    remove_role_ids: None,
                })
                .await
                .is_err()
        );
        adapter
            .add_user_to_project(AddUserToProjectInput {
                project_id: project_id.to_string().into(),
                user_id: second_id.to_string().into(),
                is_owner: Some(true),
                scopes: None,
                role_ids: None,
            })
            .await?;
        let demote_first = adapter.update_project_user(UpdateProjectUserInput {
            project_id: project_id.to_string().into(),
            user_id: first_id.to_string().into(),
            is_owner: Some(false),
            scopes: None,
            add_role_ids: None,
            remove_role_ids: None,
        });
        let demote_second = adapter.update_project_user(UpdateProjectUserInput {
            project_id: project_id.to_string().into(),
            user_id: second_id.to_string().into(),
            is_owner: Some(false),
            scopes: None,
            add_role_ids: None,
            remove_role_ids: None,
        });
        let (first_result, second_result) = tokio::join!(demote_first, demote_second);
        assert_ne!(first_result.is_ok(), second_result.is_ok());
        sqlx::query(
            "UPDATE user_projects SET is_owner = TRUE \
             WHERE project_id = $1 AND user_id IN ($2, $3)",
        )
        .bind(project_id)
        .bind(first_id)
        .bind(second_id)
        .execute(&pool)
        .await?;
        let updated_membership = adapter
            .update_project_user(UpdateProjectUserInput {
                project_id: project_id.to_string().into(),
                user_id: first_id.to_string().into(),
                is_owner: Some(false),
                scopes: Some(vec!["write_channels".to_string()]),
                add_role_ids: None,
                remove_role_ids: None,
            })
            .await?;
        assert!(!updated_membership.is_owner);
        let renamed = adapter
            .update_user(
                first.id.as_str(),
                UpdateUserInput {
                    first_name: Some("Renamed".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(renamed.first_name, "Renamed");
        assert_eq!(
            adapter
                .users(UserConnectionArgs {
                    where_filter: Some(UserWhereInput {
                        email_contains_fold: Some("PG-FIRST".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .await?
                .total_count,
            1
        );
        sqlx::query("UPDATE users SET is_owner = TRUE WHERE id = $1")
            .bind(first_id)
            .execute(&pool)
            .await?;
        assert!(
            adapter
                .update_user_status(first.id.as_str(), UserStatus::Deactivated)
                .await
                .is_err()
        );
        sqlx::query("UPDATE users SET is_owner = TRUE WHERE id = $1")
            .bind(second_id)
            .execute(&pool)
            .await?;
        assert_eq!(
            adapter
                .update_user_status(first.id.as_str(), UserStatus::Deactivated)
                .await?
                .status,
            UserStatus::Deactivated
        );
        adapter
            .update_user(
                first.id.as_str(),
                UpdateUserInput {
                    is_owner: Some(false),
                    ..Default::default()
                },
            )
            .await?;
        adapter.delete_user(first.id.as_str()).await?;
        isolated.cleanup().await?;
        Ok(())
    }
}

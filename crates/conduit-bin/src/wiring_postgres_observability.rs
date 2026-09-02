//! PostgreSQL wiring for thread/trace admin queries and Relay node lookups.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use conduit_admin_graphql::channel::{Node, OrderDirection};
use conduit_admin_graphql::node::{NodeError, NodeResolver};
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::policy::AdminAccessScope;
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_admin_graphql::threads_ext::{
    ConnectionOrderSelection, ConnectionOrderTerm, Thread, ThreadConnection, ThreadConnectionArgs,
    ThreadEdge, ThreadQueryServices, ThreadTraceError, Trace, TraceConnection, TraceConnectionArgs,
    TraceEdge, TraceQueryServices,
};
use conduit_db::repo::profile_template_repo::ProfileTemplateRepo;
use conduit_db::repo::prompt_protection_repo::PromptProtectionRuleRepo;
use conduit_db::repo::request_repo::RequestRepo;
use conduit_db::{
    ApiKeyRepo, ChannelRepo, ModelRepo, PgApiKeyRepo, PgChannelRepo, PgDataStorageRepo,
    PgModelRepo, PgProfileTemplateRepo, PgProjectRepo, PgPromptProtectionRuleRepo, PgPromptRepo,
    PgRequestRepo, PgRoleRepo, PgThreadRepo, PgTraceRepo, PgUsageRepo, PgUserRepo, PolicyContext,
    Principal, ProjectRepo, RequestContext, RoleRepo, UserRepo,
};

/// One shared adapter owns the PostgreSQL repositories needed by the
/// `threads`, `traces`, and `node` GraphQL roots.
pub(crate) struct PostgresObservabilityServices {
    pool: PgPool,
    project_repo: PgProjectRepo,
    role_repo: PgRoleRepo,
    model_repo: PgModelRepo,
    channel_repo: PgChannelRepo,
    api_key_repo: PgApiKeyRepo,
    user_repo: PgUserRepo,
    request_repo: PgRequestRepo,
    data_storage_repo: PgDataStorageRepo,
    usage_repo: PgUsageRepo,
    thread_repo: PgThreadRepo,
    trace_repo: PgTraceRepo,
    profile_template_repo: PgProfileTemplateRepo,
    prompt_repo: PgPromptRepo,
    prompt_rule_repo: PgPromptProtectionRuleRepo,
}

impl PostgresObservabilityServices {
    fn new(pool: PgPool) -> Self {
        Self {
            pool: pool.clone(),
            project_repo: PgProjectRepo::new(pool.clone()),
            role_repo: PgRoleRepo::new(pool.clone()),
            model_repo: PgModelRepo::new(pool.clone()),
            channel_repo: PgChannelRepo::new(pool.clone()),
            api_key_repo: PgApiKeyRepo::new(pool.clone()),
            user_repo: PgUserRepo::new(pool.clone()),
            request_repo: PgRequestRepo::new(pool.clone()),
            data_storage_repo: PgDataStorageRepo::new(pool.clone()),
            usage_repo: PgUsageRepo::new(pool.clone()),
            thread_repo: PgThreadRepo::new(pool.clone()),
            trace_repo: PgTraceRepo::new(pool.clone()),
            profile_template_repo: PgProfileTemplateRepo::new(pool.clone()),
            prompt_repo: PgPromptRepo::new(pool.clone()),
            prompt_rule_repo: PgPromptProtectionRuleRepo::new(pool),
        }
    }

    fn trusted_context() -> RequestContext {
        // GraphQL scope authorization has already run before a node resolver
        // is called. The repository layer still requires an internal
        // principal, matching the other host adapters' boot/system context.
        RequestContext::new(PolicyContext::new(Principal::system()))
    }

    fn load_error(error: impl std::fmt::Display) -> NodeError {
        NodeError::Load(error.to_string())
    }
}

pub(crate) fn build_postgres_observability_services(
    pool: PgPool,
) -> (
    Arc<dyn ThreadQueryServices>,
    Arc<dyn TraceQueryServices>,
    Arc<dyn NodeResolver>,
) {
    let services = Arc::new(PostgresObservabilityServices::new(pool));
    let thread_query: Arc<dyn ThreadQueryServices> = services.clone();
    let trace_query: Arc<dyn TraceQueryServices> = services.clone();
    let node_resolver: Arc<dyn NodeResolver> = services;
    (thread_query, trace_query, node_resolver)
}

fn thread_to_gql(row: conduit_db::ThreadRow) -> Thread {
    Thread {
        id: format!("gid://conduit/Thread/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        project_id: format!("gid://conduit/Project/{}", row.project_id).into(),
        thread_id: row.thread_id,
    }
}

fn trace_to_gql(row: conduit_db::TraceRow) -> Trace {
    Trace {
        id: format!("gid://conduit/Trace/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        project_id: format!("gid://conduit/Project/{}", row.project_id).into(),
        trace_id: row.trace_id,
        thread_id: row
            .thread_id
            .map(|id| format!("gid://conduit/Thread/{id}").into()),
    }
}

fn paginate<T: Clone>(
    mut nodes: Vec<T>,
    order_by: &Option<ConnectionOrderSelection>,
    after: &Option<String>,
    first: Option<i32>,
    updated_at: impl Fn(&T) -> chrono::DateTime<chrono::Utc>,
) -> (Vec<T>, i64, u64, usize) {
    if let Some(order_by) = order_by {
        if order_by.term == ConnectionOrderTerm::UpdatedAt {
            nodes.sort_by_key(updated_at);
        }
        if order_by.direction == OrderDirection::Desc {
            nodes.reverse();
        }
    }

    let total_count = nodes.len() as i64;
    let start_offset = after
        .as_deref()
        .and_then(|cursor| decode_offset_cursor(cursor).ok())
        .map(|offset| offset + 1)
        .unwrap_or_default();
    let start = usize::try_from(start_offset)
        .unwrap_or_default()
        .min(nodes.len());
    let window = nodes[start..].to_vec();
    let page_size = first
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(window.len());
    (window, total_count, start_offset, page_size)
}

#[async_trait]
impl ThreadQueryServices for PostgresObservabilityServices {
    async fn threads(
        &self,
        args: ThreadConnectionArgs,
    ) -> Result<ThreadConnection, ThreadTraceError> {
        let rows = match &args.access {
            AdminAccessScope::Global => self.thread_repo.list_all().await,
            AdminAccessScope::Project(project_id) => {
                self.thread_repo
                    .list_by_project(&database_id(project_id, "Project"))
                    .await
            }
        };
        let mut nodes: Vec<Thread> = rows
            .map_err(|error| ThreadTraceError::QueryThreads(error.to_string()))?
            .into_iter()
            .map(thread_to_gql)
            .collect();
        if let Some(filter) = args.where_filter.as_ref() {
            nodes.retain(|thread| {
                !filter.id.as_ref().is_some_and(|id| id != &thread.id)
                    && !filter
                        .project_id
                        .as_ref()
                        .is_some_and(|id| id != &thread.project_id)
                    && !filter
                        .thread_id
                        .as_ref()
                        .is_some_and(|id| id != &thread.thread_id)
                    && !filter
                        .thread_id_contains
                        .as_ref()
                        .is_some_and(|part| !thread.thread_id.contains(part))
            });
        }

        let (window, total_count, start_offset, page_size) =
            paginate(nodes, &args.order_by, &args.after, args.first, |node| {
                node.updated_at.0
            });
        let connection = connection_from_offset_page(window, start_offset, page_size);
        Ok(ThreadConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(ThreadEdge {
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
impl TraceQueryServices for PostgresObservabilityServices {
    async fn traces(&self, args: TraceConnectionArgs) -> Result<TraceConnection, ThreadTraceError> {
        let rows = match &args.access {
            AdminAccessScope::Global => self.trace_repo.list_all().await,
            AdminAccessScope::Project(project_id) => {
                self.trace_repo
                    .list_by_project(&database_id(project_id, "Project"))
                    .await
            }
        };
        let mut nodes: Vec<Trace> = rows
            .map_err(|error| ThreadTraceError::QueryTraces(error.to_string()))?
            .into_iter()
            .map(trace_to_gql)
            .collect();
        if let Some(filter) = args.where_filter.as_ref() {
            nodes.retain(|trace| {
                !filter.id.as_ref().is_some_and(|id| id != &trace.id)
                    && !filter
                        .project_id
                        .as_ref()
                        .is_some_and(|id| id != &trace.project_id)
                    && !filter
                        .thread_id
                        .as_ref()
                        .is_some_and(|id| trace.thread_id.as_ref() != Some(id))
                    && !filter
                        .trace_id
                        .as_ref()
                        .is_some_and(|id| id != &trace.trace_id)
            });
        }

        let (window, total_count, start_offset, page_size) =
            paginate(nodes, &args.order_by, &args.after, args.first, |node| {
                node.updated_at.0
            });
        let connection = connection_from_offset_page(window, start_offset, page_size);
        Ok(TraceConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(TraceEdge {
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
impl NodeResolver for PostgresObservabilityServices {
    async fn resolve_node(&self, node_type: &str, id: i64) -> Result<Option<Node>, NodeError> {
        let context = Self::trusted_context();
        let database_id = id.to_string();
        match node_type {
            "Project" => self
                .project_repo
                .find_project(&context, &database_id)
                .await
                .map(|row| {
                    row.map(crate::wiring_postgres_project_role::project_row_to_gql)
                        .map(Node::Project)
                })
                .map_err(Self::load_error),
            "Role" => self
                .role_repo
                .find_role(&context, &database_id)
                .await
                .map(|row| {
                    row.map(crate::wiring_postgres_project_role::role_row_to_gql)
                        .map(Node::Role)
                })
                .map_err(Self::load_error),
            "Model" => self
                .model_repo
                .find_model(&context, &database_id)
                .await
                .map(|row| row.map(crate::conv::model_row_to_gql).map(Node::Model))
                .map_err(Self::load_error),
            "Channel" => self
                .channel_repo
                .find_channel(&context, &database_id)
                .await
                .map(|row| row.map(crate::conv::channel_row_to_gql).map(Node::Channel))
                .map_err(Self::load_error),
            "APIKey" => self
                .api_key_repo
                .find_api_key_by_id(&context, &database_id)
                .await
                .map(|row| row.map(crate::wiring_apikey::row_to_gql).map(Node::APIKey))
                .map_err(Self::load_error),
            "APIKeyProfileTemplate" => self
                .profile_template_repo
                .find_profile_template_by_id_unchecked(&context, &database_id)
                .await
                .map(|row| {
                    row.map(crate::wiring_profile_template::template_row_to_gql)
                        .map(Node::APIKeyProfileTemplate)
                })
                .map_err(Self::load_error),
            "User" => self
                .user_repo
                .find_user_by_id(&context, &database_id)
                .await
                .map(|row| {
                    row.map(crate::wiring_postgres_user::user_to_gql)
                        .map(Node::User)
                })
                .map_err(Self::load_error),
            "UserProject" => sqlx::query_as::<_, conduit_db::UserProjectRow>(
                "SELECT CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id, \
                 CAST(project_id AS TEXT) AS project_id, is_owner, scopes, \
                 created_at, updated_at FROM user_projects WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(crate::wiring_postgres_user::user_project_to_gql)
                    .map(Node::UserProject)
            })
            .map_err(Self::load_error),
            "UserRole" => sqlx::query_as::<_, conduit_db::UserRoleRow>(
                "SELECT CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id, \
                 CAST(role_id AS TEXT) AS role_id, created_at, updated_at \
                 FROM user_roles WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(crate::wiring_postgres_user::user_role_to_gql)
                    .map(Node::UserRole)
            })
            .map_err(Self::load_error),
            "Prompt" => self
                .prompt_repo
                .find_by_row_id(id)
                .await
                .map(|row| {
                    row.map(crate::wiring_prompt::prompt_row_to_gql)
                        .map(Node::Prompt)
                })
                .map_err(Self::load_error),
            "PromptProtectionRule" => self
                .prompt_rule_repo
                .find_protection_rule_unchecked(&context, &database_id)
                .await
                .map(|row| {
                    row.map(crate::wiring_prompt::rule_row_to_gql)
                        .map(Node::PromptProtectionRule)
                })
                .map_err(Self::load_error),
            "Request" => {
                let mut row = self
                    .request_repo
                    .find_request_by_id(&context, &database_id)
                    .await
                    .map_err(Self::load_error)?;
                if let Some(row) = row.as_mut() {
                    crate::wiring_request_content::hydrate_request_artifacts(
                        &self.data_storage_repo,
                        row,
                    )
                    .await;
                }
                Ok(row
                    .map(crate::wiring_requests::request_row_to_gql)
                    .map(Node::Request))
            }
            "UsageLog" => self
                .usage_repo
                .find_by_id(id)
                .await
                .map(|row| {
                    row.map(crate::wiring_requests::usage_log_row_to_gql)
                        .map(Node::UsageLog)
                })
                .map_err(Self::load_error),
            "Thread" => self
                .thread_repo
                .find_by_row_id(id)
                .await
                .map(|row| row.map(thread_to_gql).map(Node::Thread))
                .map_err(Self::load_error),
            "Trace" => self
                .trace_repo
                .find_by_row_id(id)
                .await
                .map(|row| row.map(trace_to_gql).map(Node::Trace))
                .map_err(Self::load_error),
            other => Err(NodeError::UnknownType(other.to_owned())),
        }
    }

    async fn resolve_node_with_access(
        &self,
        node_type: &str,
        id: i64,
        access: &AdminAccessScope,
    ) -> Result<Option<Node>, NodeError> {
        if matches!(access, AdminAccessScope::Global) {
            return self.resolve_node(node_type, id).await;
        }

        let context = Self::trusted_context();
        let database_id = id.to_string();
        match node_type {
            "Project" => {
                if !access.allows_project(&database_id) {
                    return Ok(None);
                }
                self.project_repo
                    .find_project(&context, &database_id)
                    .await
                    .map(|row| {
                        row.map(crate::wiring_postgres_project_role::project_row_to_gql)
                            .map(Node::Project)
                    })
                    .map_err(Self::load_error)
            }
            "Role" => self
                .role_repo
                .find_role(&context, &database_id)
                .await
                .map(|row| {
                    row.filter(|row| {
                        row.level == "project" && access.allows_project(&row.project_id)
                    })
                    .map(crate::wiring_postgres_project_role::role_row_to_gql)
                    .map(Node::Role)
                })
                .map_err(Self::load_error),
            // Models, channels and prompt-protection rules are global
            // entities. Their per-project visibility is the read-scope check
            // performed by `node_read_access_scope`, not a row project key.
            "Model" | "Channel" | "PromptProtectionRule" => self.resolve_node(node_type, id).await,
            // APIKey nodes are resolved through the dedicated API-key access
            // policy in QueryRoot::node; fail closed if this generic seam is
            // ever called directly.
            "APIKey" => Ok(None),
            "APIKeyProfileTemplate" => self
                .profile_template_repo
                .find_profile_template_by_id_unchecked(&context, &database_id)
                .await
                .map(|row| {
                    row.filter(|row| access.allows_project(&row.project_id))
                        .map(crate::wiring_profile_template::template_row_to_gql)
                        .map(Node::APIKeyProfileTemplate)
                })
                .map_err(Self::load_error),
            "User" => {
                let Some(project_id) = access.project_id() else {
                    return Ok(None);
                };
                let member = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM user_projects \
                     WHERE user_id = $1 AND project_id = $2)",
                )
                .bind(id)
                .bind(
                    database_id_from_access(project_id)
                        .map_err(|error| NodeError::Load(error.to_owned()))?,
                )
                .fetch_one(&self.pool)
                .await
                .map_err(Self::load_error)?;
                if !member {
                    return Ok(None);
                }
                self.user_repo
                    .find_user_by_id(&context, &database_id)
                    .await
                    .map(|row| {
                        row.map(crate::wiring_postgres_user::user_to_gql)
                            .map(Node::User)
                    })
                    .map_err(Self::load_error)
            }
            "UserProject" => {
                let Some(project_id) = access.project_id() else {
                    return Ok(None);
                };
                sqlx::query_as::<_, conduit_db::UserProjectRow>(
                    "SELECT CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id, \
                     CAST(project_id AS TEXT) AS project_id, is_owner, scopes, \
                     created_at, updated_at FROM user_projects \
                     WHERE id = $1 AND project_id = $2",
                )
                .bind(id)
                .bind(
                    database_id_from_access(project_id)
                        .map_err(|error| NodeError::Load(error.to_owned()))?,
                )
                .fetch_optional(&self.pool)
                .await
                .map(|row| {
                    row.map(crate::wiring_postgres_user::user_project_to_gql)
                        .map(Node::UserProject)
                })
                .map_err(Self::load_error)
            }
            "UserRole" => {
                let Some(project_id) = access.project_id() else {
                    return Ok(None);
                };
                sqlx::query_as::<_, conduit_db::UserRoleRow>(
                    "SELECT CAST(ur.id AS TEXT) AS id, \
                     CAST(ur.user_id AS TEXT) AS user_id, \
                     CAST(ur.role_id AS TEXT) AS role_id, \
                     ur.created_at, ur.updated_at FROM user_roles ur \
                     JOIN roles r ON r.id = ur.role_id AND r.deleted_at = 0 \
                     JOIN user_projects up ON up.user_id = ur.user_id \
                       AND up.project_id = r.project_id \
                     WHERE ur.id = $1 AND r.level = 'project' AND r.project_id = $2",
                )
                .bind(id)
                .bind(
                    database_id_from_access(project_id)
                        .map_err(|error| NodeError::Load(error.to_owned()))?,
                )
                .fetch_optional(&self.pool)
                .await
                .map(|row| {
                    row.map(crate::wiring_postgres_user::user_role_to_gql)
                        .map(Node::UserRole)
                })
                .map_err(Self::load_error)
            }
            "Prompt" => self
                .prompt_repo
                .find_by_row_id(id)
                .await
                .map(|row| {
                    row.filter(|row| access.allows_project(&row.project_id))
                        .map(crate::wiring_prompt::prompt_row_to_gql)
                        .map(Node::Prompt)
                })
                .map_err(Self::load_error),
            "Request" => {
                let mut row = self
                    .request_repo
                    .find_request_by_id(&context, &database_id)
                    .await
                    .map_err(Self::load_error)?
                    .filter(|row| access.allows_project(&row.project_id));
                if let Some(row) = row.as_mut() {
                    crate::wiring_request_content::hydrate_request_artifacts(
                        &self.data_storage_repo,
                        row,
                    )
                    .await;
                }
                Ok(row
                    .map(crate::wiring_requests::request_row_to_gql)
                    .map(Node::Request))
            }
            "UsageLog" => self
                .usage_repo
                .find_by_id(id)
                .await
                .map(|row| {
                    row.filter(|row| access.allows_project(&row.project_id))
                        .map(crate::wiring_requests::usage_log_row_to_gql)
                        .map(Node::UsageLog)
                })
                .map_err(Self::load_error),
            "Thread" => self
                .thread_repo
                .find_by_row_id(id)
                .await
                .map(|row| {
                    row.filter(|row| access.allows_project(&row.project_id))
                        .map(thread_to_gql)
                        .map(Node::Thread)
                })
                .map_err(Self::load_error),
            "Trace" => self
                .trace_repo
                .find_by_row_id(id)
                .await
                .map(|row| {
                    row.filter(|row| access.allows_project(&row.project_id))
                        .map(trace_to_gql)
                        .map(Node::Trace)
                })
                .map_err(Self::load_error),
            other => Err(NodeError::UnknownType(other.to_owned())),
        }
    }
}

fn database_id_from_access(project_id: &str) -> Result<i64, &'static str> {
    database_id(project_id, "Project")
        .parse::<i64>()
        .map_err(|_| "authorized project ID is not a valid integer")
}

fn database_id(value: &str, expected_type: &str) -> String {
    conduit_admin_graphql::node::parse_guid(value)
        .ok()
        .filter(|guid| guid.typ == expected_type)
        .map_or_else(|| value.to_owned(), |guid| guid.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conduit_db::{ThreadRepo, TraceRepo};

    #[tokio::test]
    async fn postgres_threads_traces_and_node_graphql_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let thread_external_id = format!("gql-pg-thread-{suffix}");
        let trace_external_id = format!("gql-pg-trace-{suffix}");
        let edge_seed = Utc::now().timestamp_micros().unsigned_abs() as i64;
        let context = RequestContext::new(PolicyContext::new(Principal::test()));
        let thread_repo = PgThreadRepo::new(pool.clone());
        let trace_repo = PgTraceRepo::new(pool.clone());
        let thread = thread_repo
            .get_or_create_thread(
                &context,
                "91000003",
                &thread_external_id,
                "2026-08-15T00:00:00Z".into(),
            )
            .await?;
        let trace = trace_repo
            .get_or_create_trace(
                &context,
                "91000003",
                &trace_external_id,
                Some(thread.id.clone()),
                "2026-08-15T00:00:00Z".into(),
            )
            .await?;
        let template_id: i64 = sqlx::query_scalar(
            "INSERT INTO api_key_profile_templates \
             (project_id, name, description, profile) \
             VALUES ($1, $2, '', $3) RETURNING id",
        )
        .bind(edge_seed)
        .bind(format!("node-template-{suffix}"))
        .bind(sqlx::types::Json(serde_json::json!({
            "name": "Node template",
            "modelMappings": null
        })))
        .fetch_one(&pool)
        .await?;
        let prompt_id: i64 = sqlx::query_scalar(
            "INSERT INTO prompts \
             (project_id, name, role, content, settings) \
             VALUES ($1, $2, 'system', 'node test', '{}'::jsonb) RETURNING id",
        )
        .bind(edge_seed)
        .bind(format!("node-prompt-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let prompt_rule_id: i64 = sqlx::query_scalar(
            "INSERT INTO prompt_protection_rules (name, pattern, settings) \
             VALUES ($1, 'secret', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("node-rule-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let user_project_id: i64 = sqlx::query_scalar(
            "INSERT INTO user_projects (user_id, project_id, scopes) \
             VALUES ($1, $2, '[]'::jsonb) RETURNING id",
        )
        .bind(edge_seed)
        .bind(edge_seed.saturating_add(1))
        .fetch_one(&pool)
        .await?;
        let user_role_id: i64 = sqlx::query_scalar(
            "INSERT INTO user_roles (user_id, role_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now()) RETURNING id",
        )
        .bind(edge_seed)
        .bind(edge_seed.saturating_add(1))
        .fetch_one(&pool)
        .await?;

        let (thread_query, trace_query, node_resolver) =
            build_postgres_observability_services(pool.clone());
        let mut graphql_context = conduit_auth::RequestContext::new();
        let _ = graphql_context.set_principal(
            conduit_auth::Principal::user("observability-test")
                .with_scope(conduit_auth::scopes::slug::READ_REQUESTS),
        );
        let schema = conduit_admin_graphql::admin_schema_builder()
            .data(thread_query)
            .data(trace_query)
            .data(node_resolver)
            .data(graphql_context)
            .finish();
        let response = schema
            .execute(format!(
                r#"{{
                    threads(where: {{ threadID: "{thread_external_id}" }}) {{
                        totalCount edges {{ node {{ id threadID }} }}
                    }}
                    traces(where: {{ traceID: "{trace_external_id}" }}) {{
                        totalCount edges {{ node {{ id traceID threadID }} }}
                    }}
                    node(id: "gid://conduit/Trace/{}") {{
                        __typename
                        ... on Trace {{ traceID }}
                    }}
                }}"#,
                trace.id
            ))
            .await;
        assert!(response.errors.is_empty(), "errors: {:?}", response.errors);
        let data = response.data.to_string();
        assert!(data.contains(&thread_external_id), "data: {data}");
        assert!(data.contains(&trace_external_id), "data: {data}");
        assert!(data.contains("Trace"), "data: {data}");

        let resolver = PostgresObservabilityServices::new(pool.clone());
        assert!(matches!(
            resolver
                .resolve_node("APIKeyProfileTemplate", template_id)
                .await?,
            Some(Node::APIKeyProfileTemplate(_))
        ));
        assert!(matches!(
            resolver.resolve_node("Prompt", prompt_id).await?,
            Some(Node::Prompt(_))
        ));
        let prompt_access = AdminAccessScope::Project(edge_seed.to_string());
        assert!(matches!(
            resolver
                .resolve_node_with_access("Prompt", prompt_id, &prompt_access)
                .await?,
            Some(Node::Prompt(_))
        ));
        assert!(
            resolver
                .resolve_node_with_access(
                    "Prompt",
                    prompt_id,
                    &AdminAccessScope::Project(edge_seed.saturating_add(1).to_string()),
                )
                .await?
                .is_none()
        );
        assert!(matches!(
            resolver
                .resolve_node("PromptProtectionRule", prompt_rule_id)
                .await?,
            Some(Node::PromptProtectionRule(_))
        ));
        assert!(matches!(
            resolver
                .resolve_node("UserProject", user_project_id)
                .await?,
            Some(Node::UserProject(_))
        ));
        assert!(matches!(
            resolver.resolve_node("UserRole", user_role_id).await?,
            Some(Node::UserRole(_))
        ));

        sqlx::query("DELETE FROM traces WHERE id = $1")
            .bind(trace.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM threads WHERE id = $1")
            .bind(thread.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM api_key_profile_templates WHERE id = $1")
            .bind(template_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM prompts WHERE id = $1")
            .bind(prompt_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM prompt_protection_rules WHERE id = $1")
            .bind(prompt_rule_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_projects WHERE id = $1")
            .bind(user_project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_roles WHERE id = $1")
            .bind(user_role_id)
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }
}

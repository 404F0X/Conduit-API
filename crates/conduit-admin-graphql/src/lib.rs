#![forbid(unsafe_code)]

use async_graphql::{Context, EmptySubscription, Enum, ID, Object, Schema, SimpleObject};

pub mod apikey;
pub mod authz_extension;
pub mod backup_ext;
pub mod billing;
pub mod change_set;
pub mod channel;
pub mod channel_ext;
pub mod channel_ext2;
pub mod channel_override_template_ext;
pub mod channel_probe_ext;
pub mod channel_queries;
pub mod commercialization;
pub mod dashboard;
pub mod data_storage;
pub mod dataloader;
pub mod input;
pub mod me;
pub mod me_ext;
pub mod model;
pub mod model_catalog;
pub mod model_ext;
pub mod mutation;
pub mod node;
pub mod operations;
pub mod pagination;
pub mod policy;
pub mod product_experience;
pub mod profile_template;
pub mod project;
pub mod prompt;
pub mod provider_quota_ext;
pub mod query;
pub mod quota_ext;
pub mod request_execution;
pub mod request_usage;
pub mod role;
pub mod route_explanation;
pub mod scalars;
pub mod schema;
#[cfg(test)]
pub(crate) mod sdl_parity;
pub mod simple_group;
pub mod system;
pub mod system_ext;
pub mod system_operations_ext;
pub mod system_settings_ext;
pub mod threads_ext;
pub mod user;

use pagination::{PageInfo, connection_from_offset_page};
use scalars::CursorScalar;

pub const CRATE_NAME: &str = "conduit-admin-graphql";

pub type AdminSchema = Schema<QueryRoot, mutation::MutationRoot, EmptySubscription>;

pub struct QueryRoot;

#[derive(SimpleObject)]
pub struct AdminViewer {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum AutoSyncFrequency {
    OneHour,
    SixHours,
    OneDay,
}

#[derive(SimpleObject)]
pub struct GraphqlEnumCasingProbe {
    pub quota_enforcement_modes: Vec<scalars::QuotaEnforcementMode>,
    pub auto_sync_frequencies: Vec<AutoSyncFrequency>,
}

#[derive(SimpleObject)]
pub struct GraphqlConnectionProbeNode {
    pub id: String,
}

#[derive(SimpleObject)]
pub struct GraphqlConnectionProbeEdge {
    pub cursor: String,
    pub node: GraphqlConnectionProbeNode,
}

#[derive(SimpleObject)]
pub struct GraphqlConnectionProbe {
    pub edges: Vec<GraphqlConnectionProbeEdge>,
    pub page_info: PageInfo,
}

#[Object]
impl QueryRoot {
    async fn health(&self) -> &'static str {
        "ok"
    }

    async fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn change_sets(
        &self,
        ctx: &Context<'_>,
        kind: Option<change_set::ChangeSetKind>,
        status: Option<change_set::ChangeSetStatus>,
        scope_type: Option<String>,
        #[graphql(name = "scopeID")] scope_id: Option<ID>,
        limit: Option<i32>,
    ) -> Result<Vec<change_set::ChangeSet>, String> {
        change_set::change_set_services(ctx)?
            .change_sets(
                kind,
                status,
                scope_type,
                scope_id.map(|id| id.to_string()),
                limit.unwrap_or(100).clamp(1, 500),
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Credential-free routing explanation for one persisted request.
    async fn request_route_explanation(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "requestID")] request_id: ID,
    ) -> Result<Option<route_explanation::RequestRouteExplanation>, String> {
        let project_id =
            policy::request_context(ctx).and_then(|request| request.project_id.as_deref());
        route_explanation::route_explanation_services(ctx)?
            .request_route_explanation(request_id.as_str(), project_id)
            .await
            .map_err(|error| error.to_string())
    }

    /// Cross-project usage economics and channel reliability for operators.
    async fn operations_ledger(
        &self,
        ctx: &Context<'_>,
        period_days: Option<i32>,
    ) -> Result<operations::OperationsLedger, String> {
        let services = operations::operations_services(ctx)?;
        services
            .operations_ledger(period_days.unwrap_or(7))
            .await
            .map_err(|err| err.to_string())
    }

    /// Successful metered-usage paths for the operator flow visualization.
    async fn operations_flow(
        &self,
        ctx: &Context<'_>,
        period_days: Option<i32>,
        limit: Option<i32>,
    ) -> Result<operations::OperationsFlow, String> {
        operations::operations_services(ctx)?
            .operations_flow(period_days.unwrap_or(7), limit.unwrap_or(100))
            .await
            .map_err(|err| err.to_string())
    }

    /// Real time-bucketed public-model usage for operator model analytics.
    async fn operations_model_series(
        &self,
        ctx: &Context<'_>,
        period_days: Option<i32>,
    ) -> Result<operations::OperationsModelSeries, String> {
        operations::operations_services(ctx)?
            .operations_model_series(period_days.unwrap_or(1))
            .await
            .map_err(|err| err.to_string())
    }

    /// Append-only provider balance observations and upstream price changes.
    async fn provider_observation_history(
        &self,
        ctx: &Context<'_>,
        channel_id: ID,
        limit: Option<i32>,
    ) -> Result<operations::ProviderObservationHistory, String> {
        let services = operations::operations_services(ctx)?;
        services
            .provider_observation_history(channel_id.as_str(), limit.unwrap_or(50))
            .await
            .map_err(|err| err.to_string())
    }

    /// Return the active product projection for every authenticated user.
    async fn product_experience_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<product_experience::ProductExperienceSettings, String> {
        product_experience::product_experience_services(ctx)?
            .settings()
            .await
            .map_err(|error| error.to_string())
    }

    /// Personalized model and sanitized route catalog for the current user.
    async fn my_model_catalog(
        &self,
        ctx: &Context<'_>,
    ) -> Result<model_catalog::MyModelCatalog, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| model_catalog::ModelCatalogError::ProjectRequired.to_string())?
            .parse::<i64>()
            .map_err(|_| model_catalog::ModelCatalogError::ProjectRequired.to_string())?;
        model_catalog::model_catalog_services(ctx)?
            .my_model_catalog(current.user_id, project_id)
            .await
            .map_err(|error| error.to_string())
    }

    /// Simple-mode commercial façade. Unlike `userGroups`, this contract does
    /// not expose legacy model/channel arrays or User membership internals.
    async fn simple_groups(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<simple_group::SimpleGroup>, String> {
        simple_group::simple_group_services(ctx)?
            .simple_groups()
            .await
            .map_err(|error| error.to_string())
    }

    /// Model groups currently effective for the selected Project. This is a
    /// project-scoped API-key editor projection, not the administrator's
    /// cross-project group list.
    async fn api_key_assignable_groups(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<simple_group::APIKeyAssignableGroup>, String> {
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "project context is required".to_string())?
            .parse::<i64>()
            .map_err(|_| "invalid project context".to_string())?;
        simple_group::simple_group_services(ctx)?
            .api_key_assignable_groups(project_id)
            .await
            .map_err(|error| error.to_string())
    }

    /// Provider inventory discovered from configured upstream channels.
    async fn upstream_model_deployments(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: Option<ID>,
    ) -> Result<Vec<commercialization::UpstreamModelDeployment>, String> {
        commercialization::commercialization_services(ctx)?
            .upstream_model_deployments(channel_id.as_ref().map(|id| id.as_str()))
            .await
            .map_err(|error| error.to_string())
    }

    /// Routes connecting public model SKUs to concrete upstream deployments.
    async fn model_routes(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "publicModelID")] public_model_id: Option<ID>,
    ) -> Result<Vec<commercialization::ModelRoute>, String> {
        commercialization::commercialization_services(ctx)?
            .model_routes(public_model_id.as_ref().map(|id| id.as_str()))
            .await
            .map_err(|error| error.to_string())
    }

    /// Global opt-in for the route-to-channel-alias automation tool.
    async fn channel_model_mapping_automation_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<commercialization::ChannelModelMappingAutomationSettings, String> {
        commercialization::commercialization_services(ctx)?
            .channel_model_mapping_automation_settings()
            .await
            .map_err(|error| error.to_string())
    }

    /// Preview the aliases implied by explicit public-model routes for a channel.
    async fn preview_channel_model_mappings(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: ID,
    ) -> Result<commercialization::ChannelModelMappingPreview, String> {
        commercialization::commercialization_services(ctx)?
            .preview_channel_model_mappings(channel_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    /// Conduit API Rust extension: retail price books.
    async fn price_books(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<commercialization::PriceBook>, String> {
        commercialization::commercialization_services(ctx)?
            .price_books()
            .await
            .map_err(|error| error.to_string())
    }

    /// Simple-mode account context. The user identity always comes from JWT;
    /// the service returns Missing/Ambiguous instead of guessing a Project.
    async fn my_primary_project(
        &self,
        ctx: &Context<'_>,
    ) -> Result<commercialization::PrimaryProjectResolution, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        commercialization::commercialization_services(ctx)?
            .primary_project_for_user(&current.user_id.to_string())
            .await
            .map_err(|error| error.to_string())
    }

    /// Conduit API Rust extension: durable Credit plus active subscription allowance.
    async fn user_balance(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "userID")] user_id: ID,
    ) -> Result<billing::UserBalance, String> {
        billing::billing_services(ctx)?
            .user_balance(user_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    /// Project-owned shadow wallet. This is additive and does not copy legacy
    /// user Credit into the Project ledger.
    async fn project_balance(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "projectID")] project_id: ID,
    ) -> Result<billing::ProjectBalance, String> {
        billing::billing_services(ctx)?
            .project_balance(project_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    async fn project_wallet_comparison(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "projectID")] project_id: ID,
    ) -> Result<billing::ProjectWalletComparison, String> {
        billing::billing_services(ctx)?
            .project_wallet_comparison(project_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    async fn subscription_plans(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<billing::SubscriptionPlan>, String> {
        billing::billing_services(ctx)?
            .subscription_plans()
            .await
            .map_err(|error| error.to_string())
    }

    async fn user_subscriptions(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "userID")] user_id: ID,
    ) -> Result<Vec<billing::UserSubscription>, String> {
        billing::billing_services(ctx)?
            .user_subscriptions(user_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    async fn subscription_projects(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "userID")] user_id: ID,
    ) -> Result<Vec<billing::SubscriptionProjectOption>, String> {
        billing::billing_services(ctx)?
            .subscription_projects(user_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    /// Compatibility alias for the Project wallet. The authenticated user and
    /// explicitly selected Project are both required; this no longer merges
    /// legacy balances across all Projects owned by the user.
    async fn my_balance(&self, ctx: &Context<'_>) -> Result<billing::ProjectBalance, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
        billing::billing_services(ctx)?
            .user_project_balance(&current.user_id.to_string(), project_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn my_project_balance(
        &self,
        ctx: &Context<'_>,
    ) -> Result<billing::ProjectBalance, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
        let services = billing::billing_services(ctx)?;
        services
            .user_project_balance(&current.user_id.to_string(), project_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn my_project_wallet_comparison(
        &self,
        ctx: &Context<'_>,
    ) -> Result<billing::ProjectWalletComparison, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
        let services = billing::billing_services(ctx)?;
        services
            .user_project_wallet_comparison(&current.user_id.to_string(), project_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn my_subscriptions(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<billing::UserSubscription>, String> {
        let current = ctx
            .data::<me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
        billing::billing_services(ctx)?
            .user_project_subscriptions(&current.user_id.to_string(), project_id)
            .await
            .map_err(|error| error.to_string())
    }

    // Identity/system-status queries — types + service-lookup helpers live in
    // `me.rs`; the methods live here because async-graphql only permits one
    // `#[Object] impl QueryRoot` block (same pattern as system/dashboard).

    /// `Query.me: UserInfo!` — Mirrors Go resolver `Me` (`me.resolvers.go:112-127`).
    ///
    /// Go reads the current user from the request context (`contexts.GetUser`)
    /// then loads by id (`GetUserByID(ctx, userCtx.ID)`). The Rust port reads
    /// the per-request [`me::CurrentUser`] the host handler injected from the
    /// JWT auth extension, then forwards its id to the service.
    async fn me(&self, ctx: &Context<'_>) -> Result<me::UserInfo, String> {
        let services = me::me_services(ctx)?;
        let user = me::current_user(ctx)?;
        services
            .me(user.user_id)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.myProjects: [Project!]!` — Mirrors Go resolver `MyProjects`
    /// (`me.resolvers.go:130-143`).
    async fn my_projects(&self, ctx: &Context<'_>) -> Result<Vec<project::Project>, String> {
        let services = me::me_services(ctx)?;
        let user = me::current_user(ctx)?;
        services
            .my_projects(user.user_id)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.systemStatus: SystemStatus!` — Mirrors Go resolver `SystemStatus`
    /// (`system.resolvers.go:372-381`).
    async fn system_status(&self, ctx: &Context<'_>) -> Result<me::SystemStatus, String> {
        let services = me::system_status_services(ctx)?;
        services
            .system_status()
            .await
            .map_err(|err| err.to_string())
    }

    // ----- model_ext slice (GAP-B): fetch/query models + associations -----

    /// `Query.fetchModels` — Mirrors Go resolver (model.resolvers.go:104-126).
    async fn fetch_models(
        &self,
        ctx: &Context<'_>,
        input: model_ext::FetchModelsInput,
    ) -> Result<model_ext::FetchModelsPayload, String> {
        let s = model_ext::model_ext_services(ctx)?;
        s.fetch_models(input).await.map_err(|e| e.to_string())
    }

    /// `Query.queryModels` — Mirrors Go resolver (model.resolvers.go:128-169).
    async fn query_models(
        &self,
        ctx: &Context<'_>,
        input: model_ext::QueryModelsInput,
    ) -> Result<Vec<model_ext::ModelIdentityWithStatus>, String> {
        let s = model_ext::model_ext_services(ctx)?;
        s.query_models(input).await.map_err(|e| e.to_string())
    }

    /// `Query.queryModelChannelConnections` — Go (model.resolvers.go:171-174).
    async fn query_model_channel_connections(
        &self,
        ctx: &Context<'_>,
        associations: Vec<model::ModelAssociationInput>,
    ) -> Result<Vec<model_ext::ModelChannelConnection>, String> {
        let s = model_ext::model_ext_services(ctx)?;
        s.query_model_channel_connections(associations)
            .await
            .map_err(|e| e.to_string())
    }

    /// `Query.queryUnassociatedChannels` — Go (model.resolvers.go:176-179).
    async fn query_unassociated_channels(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<model_ext::UnassociatedChannel>, String> {
        let s = model_ext::model_ext_services(ctx)?;
        s.query_unassociated_channels()
            .await
            .map_err(|e| e.to_string())
    }

    async fn enum_casing_probe(&self) -> GraphqlEnumCasingProbe {
        GraphqlEnumCasingProbe {
            quota_enforcement_modes: vec![
                scalars::QuotaEnforcementMode::ExhaustedOnly,
                scalars::QuotaEnforcementMode::DePrioritize,
            ],
            auto_sync_frequencies: vec![
                AutoSyncFrequency::OneHour,
                AutoSyncFrequency::SixHours,
                AutoSyncFrequency::OneDay,
            ],
        }
    }

    async fn connection_probe(&self) -> GraphqlConnectionProbe {
        let connection =
            connection_from_offset_page(Vec::<GraphqlConnectionProbeNode>::new(), 0, 25);

        GraphqlConnectionProbe {
            edges: connection
                .edges
                .into_iter()
                .map(|edge| GraphqlConnectionProbeEdge {
                    cursor: edge.cursor,
                    node: edge.node,
                })
                .collect(),
            page_info: connection.page_info,
        }
    }

    /// `Query.channels` — ent connection query over channels. Mirrors the Go
    /// resolver `Query.channels` (`internal/server/gql/ent.resolvers.go:327`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved) and delegate pagination + filtering to
    /// the injected [`channel::ChannelQueryServices`] (ent in Go).
    ///
    /// Contract (snapshot `type Query`, lines 5279-5309):
    /// `channels(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: ChannelOrder, where: ChannelWhereInput): ChannelConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn channels(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<channel::ChannelOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<channel::ChannelWhereInput>,
    ) -> Result<channel::ChannelConnection, String> {
        let services = channel::channel_query_services(ctx)?;
        let args = channel::ChannelConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: channel::resolve_channel_order(order_by),
            where_filter,
        };
        services.channels(args).await.map_err(|err| err.to_string())
    }

    /// `Query.channelOverrideTemplates` — the authenticated user's templates.
    #[allow(clippy::too_many_arguments)]
    async fn channel_override_templates(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<channel_override_template_ext::ChannelOverrideTemplateOrder>,
        #[graphql(name = "where")] where_filter: Option<
            channel_override_template_ext::ChannelOverrideTemplateWhereInput,
        >,
    ) -> Result<channel_override_template_ext::ChannelOverrideTemplateConnection, String> {
        let services = channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        let user = me::current_user(ctx)?;
        services
            .list(
                user.user_id,
                channel_override_template_ext::ChannelOverrideTemplateConnectionArgs {
                    after: after.map(|cursor| cursor.0),
                    first,
                    before: before.map(|cursor| cursor.0),
                    last,
                    order_by,
                    where_filter,
                },
            )
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.channelProbeData` — aligned probe samples for the requested channels.
    async fn channel_probe_data(
        &self,
        ctx: &Context<'_>,
        input: channel_probe_ext::GetChannelProbeDataInput,
    ) -> Result<Vec<channel_probe_ext::ChannelProbeData>, String> {
        channel_probe_ext::channel_probe_services(ctx)
            .map_err(|error| error.to_string())?
            .channel_probe_data(input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Sanitized aggregate health for authenticated users. The adapter returns
    /// `None` while owner-controlled exposure is disabled.
    async fn public_channel_health(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<channel_probe_ext::PublicChannelHealth>, String> {
        channel_probe_ext::channel_probe_services(ctx)
            .map_err(|error| error.to_string())?
            .public_channel_health()
            .await
            .map_err(|error| error.to_string())
    }

    async fn public_channel_health_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<channel_probe_ext::PublicChannelHealthSettings, String> {
        channel_probe_ext::channel_probe_services(ctx)
            .map_err(|error| error.to_string())?
            .public_channel_health_settings()
            .await
            .map_err(|error| error.to_string())
    }

    async fn preview_gc_cleanup(
        &self,
        ctx: &Context<'_>,
        input: system_operations_ext::TriggerGcCleanupInput,
    ) -> Result<Vec<system_operations_ext::GcCleanupPreviewItem>, String> {
        system_operations_ext::system_operations_services(ctx)
            .map_err(|error| error.to_string())?
            .preview_gc_cleanup(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn get_cache_diagnostics(
        &self,
        ctx: &Context<'_>,
        input: Option<system_operations_ext::GetCacheDiagnosticsInput>,
    ) -> Result<system_operations_ext::GetCacheDiagnosticsPayload, String> {
        system_operations_ext::system_operations_services(ctx)
            .map_err(|error| error.to_string())?
            .get_cache_diagnostics(input)
            .await
            .map_err(|error| error.to_string())
    }

    /// `Query.models` — ent connection query over models. Mirrors the Go
    /// resolver `Query.models` (`internal/server/gql/ent.resolvers.go:371`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — lines 372-374) and delegate
    /// pagination + filtering to the injected
    /// [`model::ModelQueryServices`] (ent in Go).
    ///
    /// Contract (snapshot `type Query`, lines 5372-5402):
    /// `models(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: ModelOrder, where: ModelWhereInput): ModelConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn models(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<model::ModelOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<model::ModelWhereInput>,
    ) -> Result<model::ModelConnection, String> {
        let services = model::model_query_services(ctx)?;
        let args = model::ModelConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: model::resolve_model_order(order_by),
            where_filter,
        };
        services.models(args).await.map_err(|err| err.to_string())
    }

    /// `Query.apiKeys` — ent connection query over api keys. Mirrors the Go
    /// resolver `Query.apiKeys` (`internal/server/gql/ent.resolvers.go:295`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — gql_pagination.go:413) and delegate
    /// pagination + filtering to the injected [`apikey::ApiKeyQueryServices`]
    /// (ent in Go).
    ///
    /// Contract (snapshot `type Query`, lines 5217-5247):
    /// `apiKeys(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: APIKeyOrder, where: APIKeyWhereInput): APIKeyConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn api_keys(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<apikey::APIKeyOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<apikey::APIKeyWhereInput>,
    ) -> Result<apikey::APIKeyConnection, String> {
        let services = apikey::apikey_query_services(ctx)?;
        let scope = apikey::api_key_access_scope(ctx)?;
        let args = apikey::APIKeyConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: apikey::resolve_apikey_order(order_by),
            where_filter,
        };
        services
            .api_keys(&scope, args)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.apiKeyProfileTemplates` — ent connection query over API key
    /// profile templates. Mirrors the Go resolver
    /// `Query.apiKeyProfileTemplates`
    /// (`internal/server/gql/ent.resolvers.go:310`): lower the `orderBy`
    /// argument (remapping `CREATED_AT` to the default ID order, direction
    /// preserved — gql_pagination.go:413) and delegate pagination + filtering
    /// to the injected [`profile_template::ProfileTemplateQueryServices`]
    /// (ent in Go).
    ///
    /// Contract (snapshot `type Query`, lines 5248-5278):
    /// `apiKeyProfileTemplates(after: Cursor, first: Int, before: Cursor,
    /// last: Int, orderBy: APIKeyProfileTemplateOrder, where:
    /// APIKeyProfileTemplateWhereInput): APIKeyProfileTemplateConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn api_key_profile_templates(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<profile_template::APIKeyProfileTemplateOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<
            profile_template::APIKeyProfileTemplateWhereInput,
        >,
    ) -> Result<profile_template::APIKeyProfileTemplateConnection, String> {
        let services = profile_template::profile_template_query_services(ctx)?;
        let args = profile_template::APIKeyProfileTemplateConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: profile_template::resolve_profile_template_order(order_by),
            where_filter,
        };
        services
            .api_key_profile_templates(args)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.projects` — ent connection query over projects. Mirrors the Go
    /// resolver `Query.projects` (`internal/server/gql/ent.resolvers.go:394`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — ent.resolvers.go:399-401) and delegate
    /// pagination + filtering to the injected [`project::ProjectQueryServices`]
    /// (ent in Go).
    ///
    /// Contract (snapshot `type Query`, lines 5434-5464):
    /// `projects(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: ProjectOrder, where: ProjectWhereInput): ProjectConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn projects(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<project::ProjectOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<project::ProjectWhereInput>,
    ) -> Result<project::ProjectConnection, String> {
        let services = project::project_query_services(ctx)?;
        let args = project::ProjectConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: project::resolve_project_order(order_by),
            where_filter,
        };
        services.projects(args).await.map_err(|err| err.to_string())
    }

    /// `Query.prompts` — ent connection query over prompts. Mirrors the Go
    /// resolver `Query.prompts` (`internal/server/gql/ent.resolvers.go:410`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — lines 413-415) and delegate
    /// pagination + filtering to the injected [`prompt::PromptQueryServices`].
    ///
    /// Contract (snapshot `type Query`, lines 5465-5495):
    /// `prompts(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: PromptOrder, where: PromptWhereInput): PromptConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn prompts(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<prompt::PromptOrder>,
        #[graphql(name = "where")] where_filter: Option<prompt::PromptWhereInput>,
    ) -> Result<prompt::PromptConnection, String> {
        let services = prompt::prompt_query_services(ctx)?;
        let args = prompt::PromptConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: prompt::resolve_prompt_order(order_by),
            where_filter,
        };
        services.prompts(args).await.map_err(|err| err.to_string())
    }

    /// `Query.promptProtectionRules` — ent connection query over prompt
    /// protection rules. Mirrors the Go resolver
    /// `Query.promptProtectionRules`
    /// (`internal/server/gql/ent.resolvers.go:425`): lower the `orderBy`
    /// argument (remapping `CREATED_AT` to the default ID order, direction
    /// preserved — lines 427-429) and delegate pagination + filtering to the
    /// injected [`prompt::PromptProtectionRuleQueryServices`].
    ///
    /// Contract (snapshot `type Query`, lines 5496-5525):
    /// `promptProtectionRules(after: Cursor, first: Int, before: Cursor,
    /// last: Int, orderBy: PromptProtectionRuleOrder, where:
    /// PromptProtectionRuleWhereInput): PromptProtectionRuleConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn prompt_protection_rules(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<prompt::PromptProtectionRuleOrder>,
        #[graphql(name = "where")] where_filter: Option<prompt::PromptProtectionRuleWhereInput>,
    ) -> Result<prompt::PromptProtectionRuleConnection, String> {
        let services = prompt::prompt_protection_rule_query_services(ctx)?;
        let args = prompt::PromptProtectionRuleConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: prompt::resolve_prompt_protection_rule_order(order_by),
            where_filter,
        };
        services
            .prompt_protection_rules(args)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.roles` — ent connection query over roles. Mirrors the Go
    /// resolver `Query.roles` (`internal/server/gql/ent.resolvers.go:458`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — ent.resolvers.go:462-464) and delegate
    /// pagination + filtering to the injected [`role::RoleQueryServices`].
    ///
    /// Contract (snapshot `type Query`, lines 5558-5587):
    /// `roles(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: RoleOrder, where: RoleWhereInput): RoleConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn roles(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<role::RoleOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<role::RoleWhereInput>,
    ) -> Result<role::RoleConnection, String> {
        let services = role::role_query_services(ctx)?;
        let args = role::RoleConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: role::resolve_role_order(order_by),
            where_filter,
        };
        services.roles(args).await.map_err(|err| err.to_string())
    }

    /// `Query.users` — ent connection query over users. Mirrors the Go
    /// resolver `Query.users` (`internal/server/gql/ent.resolvers.go:534`):
    /// lower the `orderBy` argument (remapping `CREATED_AT` to the default
    /// ID order, direction preserved — ent.resolvers.go:538-540) and delegate
    /// pagination + filtering to the injected [`user::UserQueryServices`].
    ///
    /// Contract (snapshot `type Query`, lines 5713-5742):
    /// `users(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: UserOrder, where: UserWhereInput): UserConnection!`.
    // The 8-parameter signature is fixed by the GraphQL contract.
    #[allow(clippy::too_many_arguments)]
    async fn users(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<user::UserOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<user::UserWhereInput>,
    ) -> Result<user::UserConnection, String> {
        let services = user::user_query_services(ctx)?;
        let args = user::UserConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: user::resolve_user_order(order_by),
            where_filter,
        };
        services.users(args).await.map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Data Storage connection (RUST-P12-001 S07, data_storage slice).
    // Contract: snapshot `type Query` lines 5341-5371. Semantics and tests
    // live in `crate::data_storage`.
    // -----------------------------------------------------------------

    /// `Query.dataStorages` — ent connection query over data storages.
    /// Mirrors Go resolver `Query.dataStorages` (ent.resolvers.go): lower
    /// the `orderBy` argument (remapping `CREATED_AT` to the default ID
    /// order, direction preserved — gql_pagination.go:413) and delegate
    /// pagination + filtering to the injected
    /// [`data_storage::DataStorageQueryServices`].
    ///
    /// Contract (snapshot lines 5341-5371):
    /// `dataStorages(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: DataStorageOrder, where: DataStorageWhereInput):
    /// DataStorageConnection!`.
    #[allow(clippy::too_many_arguments)]
    async fn data_storages(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<data_storage::DataStorageOrder>,
        #[graphql(name = "where")] where_filter: Option<data_storage::DataStorageWhereInput>,
    ) -> Result<data_storage::DataStorageConnection, String> {
        let services = data_storage::data_storage_query_services(ctx)?;
        let args = data_storage::DataStorageConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: data_storage::resolve_data_storage_order(order_by),
            where_filter,
        };
        services
            .data_storages(args)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Request + UsageLog connection queries (RUST-P12-001 S07,
    // request_usage slice). Contract: snapshot `type Query` lines
    // 5527-5557 (requests) / 5682-5712 (usageLogs). Semantics and tests
    // live in `crate::request_usage`.
    // -----------------------------------------------------------------

    /// `Query.requests` — ent connection query over requests. Mirrors Go
    /// resolver `Query.requests` (ent.resolvers.go): lower the `orderBy`
    /// argument (remapping `CREATED_AT` to the default ID order, direction
    /// preserved) and delegate pagination + filtering to the injected
    /// [`request_usage::RequestQueryServices`].
    ///
    /// Contract (snapshot lines 5527-5557):
    /// `requests(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: RequestOrder, where: RequestWhereInput): RequestConnection!`.
    #[allow(clippy::too_many_arguments)]
    async fn requests(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<request_usage::RequestOrder>,
        #[graphql(name = "where")] where_filter: Option<request_usage::RequestWhereInput>,
    ) -> Result<request_usage::RequestConnection, String> {
        let services = request_usage::request_query_services(ctx)?;
        let args = request_usage::RequestConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: request_usage::resolve_request_order(order_by),
            where_filter,
        };
        services.requests(args).await.map_err(|err| err.to_string())
    }

    /// `Query.usageLogs` — ent connection query over usage logs. Mirrors
    /// Go resolver `Query.usageLogs` (ent.resolvers.go): lower the `orderBy`
    /// argument (remapping `CREATED_AT` to the default ID order, direction
    /// preserved) and delegate pagination + filtering to the injected
    /// [`request_usage::UsageLogQueryServices`].
    ///
    /// Contract (snapshot lines 5682-5712):
    /// `usageLogs(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: UsageLogOrder, where: UsageLogWhereInput):
    /// UsageLogConnection!`.
    #[allow(clippy::too_many_arguments)]
    async fn usage_logs(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<request_usage::UsageLogOrder>,
        #[graphql(name = "where")] where_filter: Option<request_usage::UsageLogWhereInput>,
    ) -> Result<request_usage::UsageLogConnection, String> {
        let services = request_usage::usage_log_query_services(ctx)?;
        let args = request_usage::UsageLogConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: request_usage::resolve_usage_log_order(order_by),
            where_filter,
        };
        services
            .usage_logs(args)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // System settings basics (RUST-P12-001 S07, system slice). Contract:
    // snapshot `extend type Query` lines 9776-9778/9783-9784. Semantics
    // and tests live in `crate::system`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Query.systemVersion` (system.resolvers.go:482-485):
    /// return the build/version info.
    async fn system_version(&self, ctx: &Context<'_>) -> Result<system::SystemVersion, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .system_version()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.checkForUpdate` (system.resolvers.go:487-500).
    async fn check_for_update(&self, ctx: &Context<'_>) -> Result<system::VersionCheck, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .check_for_update()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.proxyPresets` (system.resolvers.go:532-540).
    async fn proxy_presets(&self, ctx: &Context<'_>) -> Result<Vec<system::ProxyPreset>, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .proxy_presets()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.securitySettings` (system.resolvers.go:527-530).
    async fn security_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system::SecuritySettings, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .security_settings()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.onboardingInfo` (system.resolvers.go:449-480):
    /// returns `null` when the service yields no record (Go: `info == nil`).
    async fn onboarding_info(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<system::OnboardingInfo>, String> {
        let services = system::system_settings_services(ctx)?;
        let record = services
            .onboarding_record()
            .await
            .map_err(|err| err.to_string())?;
        Ok(record.map(system::onboarding_info_from_record))
    }

    // -----------------------------------------------------------------
    // RUST-P12-001 S07 (continuation) — five additional settings domains.
    // Each query mirrors the Go resolver in `system.resolvers.go`. Service
    // semantics live in `crate::system`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Query.brandSettings` (system.resolvers.go:383-405):
    /// independently read brand_name / brand_logo / title and return the
    /// three-field object.
    async fn brand_settings(&self, ctx: &Context<'_>) -> Result<system::BrandSettings, String> {
        let services = system::system_settings_services(ctx)?;
        let brand_name = services.brand_name().await.map_err(|err| err.to_string())?;
        let brand_logo = services.brand_logo().await.map_err(|err| err.to_string())?;
        let title = services.title().await.map_err(|err| err.to_string())?;
        Ok(system::BrandSettings {
            brand_name: Some(brand_name),
            brand_logo: Some(brand_logo),
            title: Some(title),
        })
    }

    /// Mirrors Go `Query.storagePolicy` (system.resolvers.go:407-410).
    async fn storage_policy(&self, ctx: &Context<'_>) -> Result<system::StoragePolicy, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .storage_policy()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.retryPolicy` (system.resolvers.go:412-415).
    async fn retry_policy(&self, ctx: &Context<'_>) -> Result<system::RetryPolicy, String> {
        let services = system::system_settings_services(ctx)?;
        services.retry_policy().await.map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.userAgentPassThroughSettings`
    /// (system.resolvers.go:542-552).
    async fn user_agent_pass_through_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system::UserAgentPassThroughSettings, String> {
        let services = system::system_settings_services(ctx)?;
        let enabled = services
            .user_agent_pass_through()
            .await
            .map_err(|err| err.to_string())?;
        Ok(system::UserAgentPassThroughSettings { enabled })
    }

    /// Mirrors Go `Query.defaultDataStorageID` (system.resolvers.go:432-447):
    /// return `null` when the service yields `0`, otherwise the GUID wire
    /// form `gid://conduit/DataStorage/<id>`.
    #[graphql(name = "defaultDataStorageID")]
    async fn default_data_storage_id(&self, ctx: &Context<'_>) -> Result<Option<ID>, String> {
        let services = system::system_settings_services(ctx)?;
        let id = services
            .default_data_storage_id()
            .await
            .map_err(|err| err.to_string())?;
        if id == 0 {
            Ok(None)
        } else {
            Ok(Some(ID::from(format!("gid://conduit/DataStorage/{id}"))))
        }
    }

    /// Mirrors Go `Query.systemGeneralSettings` (system.resolvers.go:512-515):
    /// return the persisted general settings (or the default on not-found).
    async fn system_general_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system::SystemGeneralSettings, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .general_settings()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.systemModelSettings` (system.resolvers.go:422-430):
    /// return the persisted model settings (or the default on not-found).
    async fn system_model_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system::SystemModelSettings, String> {
        let services = system::system_settings_services(ctx)?;
        services
            .model_settings()
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-D — system channel settings + pass-through + scopes catalog.
    // Each query mirrors the Go resolver; types + service trait + tests
    // live in `crate::system_ext`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Query.systemChannelSettings` (system.resolvers.go:503-510):
    /// return the persisted channel probe + auto-sync settings.
    async fn system_channel_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system_ext::SystemChannelSettings, String> {
        let services = system_ext::system_channel_services(ctx)?;
        services
            .channel_setting()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.passThroughSettings` (system.resolvers.go:555-563):
    /// return the global request pass-through toggle.
    async fn pass_through_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system_ext::PassThroughSettings, String> {
        let services = system_ext::system_channel_services(ctx)?;
        let enabled = services
            .pass_through()
            .await
            .map_err(|err| err.to_string())?;
        Ok(system_ext::PassThroughSettings { enabled })
    }

    /// Mirrors Go `Query.allScopes` (scopes.resolvers.go:15-40): the scope
    /// catalog, optionally filtered by level. Pure — no service dependency.
    async fn all_scopes(
        &self,
        _ctx: &Context<'_>,
        level: Option<String>,
    ) -> Vec<system_ext::ScopeInfo> {
        system_ext::all_scopes(level.as_deref())
    }

    // -----------------------------------------------------------------
    // GAP-03 — channel list-page extended queries. Types + service trait
    // + reference bodies live in `crate::channel_queries`; these four
    // delegates forward to the injected `ChannelExtraQueryServices`.
    // -----------------------------------------------------------------

    /// `Query.allChannelSummarys(includeArchived: Boolean): [Channel!]!`
    /// — Go `conduit.resolvers.go:674-701`.
    async fn all_channel_summarys(
        &self,
        ctx: &Context<'_>,
        include_archived: Option<bool>,
    ) -> Result<Vec<channel::Channel>, String> {
        let services = channel_queries::channel_extra_query_services(ctx)?;
        services
            .all_channel_summarys(include_archived.unwrap_or(false))
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.allChannelTags: [String!]!` — Go `conduit.resolvers.go:704-731`.
    async fn all_channel_tags(&self, ctx: &Context<'_>) -> Result<Vec<String>, String> {
        let services = channel_queries::channel_extra_query_services(ctx)?;
        services
            .all_channel_tags()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.countChannelsByType(input): [ChannelTypeCount!]!`
    /// — Go `conduit.resolvers.go:733-763`.
    async fn count_channels_by_type(
        &self,
        ctx: &Context<'_>,
        input: channel_queries::CountChannelsByTypeInput,
    ) -> Result<Vec<channel_queries::ChannelTypeCount>, String> {
        let services = channel_queries::channel_extra_query_services(ctx)?;
        services
            .count_channels_by_type(input.into())
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.queryChannels(input): ChannelConnection!`
    /// — Go `conduit.resolvers.go:765-770` → `biz.ChannelService.QueryChannels`.
    async fn query_channels(
        &self,
        ctx: &Context<'_>,
        input: channel_queries::QueryChannelInput,
    ) -> Result<channel::ChannelConnection, String> {
        let services = channel_queries::channel_extra_query_services(ctx)?;
        services
            .query_channels(input.into())
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-E — threads / traces ent connections. Both mirror the Go
    // resolvers in `ent.resolvers.go`; types + service trait + tests live
    // in `crate::threads_ext`.
    // -----------------------------------------------------------------

    /// `Query.threads` — ent connection over threads. Mirrors Go
    /// `queryResolver.Threads` (ent.resolvers.go:486-500): validate the
    /// pagination args, remap a `CREATED_AT` ordering to ent's default
    /// (order by ID, direction preserved), then delegate pagination +
    /// filtering to the injected [`threads_ext::ThreadQueryServices`].
    ///
    /// Contract (snapshot lines 5620-5650):
    /// `threads(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: ThreadOrder, where: ThreadWhereInput): ThreadConnection!`.
    #[allow(clippy::too_many_arguments)]
    async fn threads(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<threads_ext::ThreadOrder>,
        // `where` is a Rust keyword; the GraphQL argument name is pinned.
        #[graphql(name = "where")] where_filter: Option<threads_ext::ThreadWhereInput>,
    ) -> Result<threads_ext::ThreadConnection, String> {
        let services = threads_ext::thread_query_services(ctx)?;
        let args = threads_ext::ThreadConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: threads_ext::resolve_thread_order(order_by),
            where_filter,
        };
        services.threads(args).await.map_err(|err| err.to_string())
    }

    /// `Query.traces` — ent connection over traces. Mirrors Go
    /// `queryResolver.Traces` (ent.resolvers.go:502-516): same shape as
    /// `threads`, delegating to [`threads_ext::TraceQueryServices`].
    ///
    /// Contract (snapshot lines 5651-5681):
    /// `traces(after: Cursor, first: Int, before: Cursor, last: Int,
    /// orderBy: TraceOrder, where: TraceWhereInput): TraceConnection!`.
    #[allow(clippy::too_many_arguments)]
    async fn traces(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<threads_ext::TraceOrder>,
        #[graphql(name = "where")] where_filter: Option<threads_ext::TraceWhereInput>,
    ) -> Result<threads_ext::TraceConnection, String> {
        let services = threads_ext::trace_query_services(ctx)?;
        let args = threads_ext::TraceConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: threads_ext::resolve_trace_order(order_by),
            where_filter,
        };
        services.traces(args).await.map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-F — quota read side. Types + service trait + tests live in
    // `crate::quota_ext`; the host injects a `QuotaQueryServices`
    // implementation. Go source: system.resolvers.go:523 +
    // conduit.resolvers.go:773.
    // -----------------------------------------------------------------

    /// Mirrors Go `Query.quotaEnforcementSettings` (system.resolvers.go:523-525):
    /// return the persisted quota-enforcement settings (or the default on
    /// not-found — the Go service branch returns the default).
    async fn quota_enforcement_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<mutation::QuotaEnforcementSettings, String> {
        let services = quota_ext::quota_query_services(ctx)?;
        services
            .quota_enforcement_settings()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.apiKeyQuotaUsages` (conduit.resolvers.go:773-802):
    /// look up the api key by its GUID and return the per-profile quota-usage
    /// rows (profile name + configured quota + rolling window + observed usage).
    #[graphql(name = "apiKeyQuotaUsages")]
    async fn api_key_quota_usages(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "apiKeyId")] api_key_id: ID,
    ) -> Result<Vec<quota_ext::ApiKeyProfileQuotaUsage>, String> {
        let services = quota_ext::quota_query_services(ctx)?;
        services
            .api_key_quota_usages(api_key_id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-D (remainder) — video-storage / webhook / auto-backup settings
    // reads. Types + service trait + tests live in
    // `crate::system_settings_ext`; Go source is `system.resolvers.go`
    // (video/webhook) + `backup.resolvers.go` (autoBackup).
    // -----------------------------------------------------------------

    /// Mirrors Go `Query.videoStorageSettings` (system.resolvers.go:518):
    /// return the persisted video-storage settings (or the default).
    async fn video_storage_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system_settings_ext::VideoStorageSettings, String> {
        let services = system_settings_ext::system_settings_ext_services(ctx)?;
        services
            .video_storage_settings()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.webhookNotifierConfig` (system.resolvers.go:418):
    /// return the persisted webhook targets + subscriptions.
    async fn webhook_notifier_config(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system_settings_ext::WebhookNotifierConfig, String> {
        let services = system_settings_ext::system_settings_ext_services(ctx)?;
        services
            .webhook_notifier_config()
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Query.autoBackupSettings` (backup.resolvers.go:140):
    /// return the persisted auto-backup settings (or the default).
    async fn auto_backup_settings(
        &self,
        ctx: &Context<'_>,
    ) -> Result<system_settings_ext::AutoBackupSettings, String> {
        let services = system_settings_ext::system_settings_ext_services(ctx)?;
        services
            .auto_backup_settings()
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // RUST-P12-001 S07 (continuation) — global Relay node/nodes queries.
    // Go source: `internal/server/gql/ent.resolvers.go:279-292`. Semantics
    // and tests live in `crate::node`.
    // -----------------------------------------------------------------

    /// `Query.node(id: ID!): Node` — Mirrors Go `queryResolver.Node`
    /// (ent.resolvers.go:280-287): parse the `gid://conduit/<Type>/<ID>`
    /// wire form, dispatch by type to the host-injected
    /// [`node::NodeResolver`], and return `null` for missing rows.
    async fn node(&self, ctx: &Context<'_>, id: ID) -> Result<Option<channel::Node>, String> {
        if node::parse_guid(id.as_str()).is_ok_and(|guid| guid.typ == "APIKey") {
            let scope = apikey::api_key_access_scope(ctx)?;
            return apikey::apikey_query_services(ctx)?
                .api_key(&scope, id.as_str())
                .await
                .map(|key| key.map(channel::Node::APIKey))
                .map_err(|error| error.to_string());
        }
        node::resolve_single(ctx, &id).await
    }

    /// `Query.nodes(ids: [ID!]!): [Node]!` — Mirrors Go `queryResolver.Nodes`
    /// (ent.resolvers.go:290-292). Unlike the Go counterpart (which
    /// `panic`s "not implemented"), this implementation dispatches each id
    /// to the host-injected [`node::NodeResolver`] and returns one slot
    /// per input id, with `null` for missing rows.
    async fn nodes(
        &self,
        ctx: &Context<'_>,
        ids: Vec<ID>,
    ) -> Result<Vec<Option<channel::Node>>, String> {
        let mut resolved = Vec::with_capacity(ids.len());
        for id in ids {
            resolved.push(self.node(ctx, id).await?);
        }
        Ok(resolved)
    }

    // -----------------------------------------------------------------
    // RUST-P12-001 S07 (continuation) — Dashboard statistics slice.
    // Go source: `internal/server/gql/dashboard.resolvers.go`. Semantics
    // and tests live in `crate::dashboard`.
    // -----------------------------------------------------------------

    /// `Query.dashboardOverview: DashboardOverview!` — Mirrors Go resolver
    /// `DashboardOverview` (`dashboard.resolvers.go:38-85`). Aggregates
    /// `request.status` counts (total + failed) and embeds the
    /// [`dashboard::RequestStats`] sub-object. `averageResponseTime` is
    /// currently always `None` in Go (TODO at line 81) and is mirrored here.
    async fn dashboard_overview(
        &self,
        ctx: &Context<'_>,
    ) -> Result<dashboard::DashboardOverview, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .dashboard_overview()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.requestStats: RequestStats!` — Mirrors Go resolver
    /// `RequestStats` (`dashboard.resolvers.go:91-138`): four
    /// calendar-period `COUNT(*)` values over `usage_logs`.
    async fn request_stats(&self, ctx: &Context<'_>) -> Result<dashboard::RequestStats, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .request_stats()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.tokenStats: TokenStats!` — Mirrors Go resolver `TokenStats`
    /// (`dashboard.resolvers.go:684-882`): twelve token sums (today / this
    /// week / this month / all-time x input/output/cached) and the
    /// `lastUpdated` cache timestamp.
    async fn token_stats(&self, ctx: &Context<'_>) -> Result<dashboard::TokenStats, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services.token_stats().await.map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // RUST-P12-001 S07 (continuation) — remaining dashboard analytics.
    // Each query mirrors the Go resolver in `dashboard.resolvers.go`.
    // -----------------------------------------------------------------

    /// `Query.requestStatsByChannel(timeWindow: String): [RequestStatsByChannel!]!`
    async fn request_stats_by_channel(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::RequestStatsByChannel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .request_stats_by_channel(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.requestStatsByModel(timeWindow: String): [RequestStatsByModel!]!`
    async fn request_stats_by_model(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::RequestStatsByModel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .request_stats_by_model(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.requestStatsByAPIKey(timeWindow: String): [RequestStatsByAPIKey!]!`
    #[graphql(name = "requestStatsByAPIKey")]
    async fn request_stats_by_api_key(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::RequestStatsByAPIKey>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .request_stats_by_api_key(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.tokenStatsByAPIKey(timeWindow: String): [TokenStatsByAPIKey!]!`
    #[graphql(name = "tokenStatsByAPIKey")]
    async fn token_stats_by_api_key(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::TokenStatsByAPIKey>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .token_stats_by_api_key(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.apiKeyTokenUsageStats(input: APIKeyTokenUsageStatsInput): [APIKeyTokenUsageStats!]!`
    async fn api_key_token_usage_stats(
        &self,
        ctx: &Context<'_>,
        input: Option<dashboard::APIKeyTokenUsageStatsInput>,
    ) -> Result<Vec<dashboard::APIKeyTokenUsageStats>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .api_key_token_usage_stats(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.dailyRequestStats: [DailyRequestStats!]!`
    async fn daily_request_stats(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<dashboard::DailyRequestStats>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .daily_request_stats()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.hourlyRequestStats(date: String): [HourlyRequestStats!]!`
    async fn hourly_request_stats(
        &self,
        ctx: &Context<'_>,
        date: Option<String>,
    ) -> Result<Vec<dashboard::HourlyRequestStats>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .hourly_request_stats(date)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.topRequestsProjects: [TopRequestsProjects!]!`
    async fn top_requests_projects(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<dashboard::TopRequestsProjects>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .top_requests_projects()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.channelSuccessRates(timeWindow: String, limit: Int): [ChannelSuccessRate!]!`
    async fn channel_success_rates(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
        limit: Option<i32>,
    ) -> Result<Vec<dashboard::ChannelSuccessRate>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .channel_success_rates(time_window, limit)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.fastestChannels(input: FastestChannelsInput!): [FastestChannel!]!`
    async fn fastest_channels(
        &self,
        ctx: &Context<'_>,
        input: dashboard::FastestChannelsInput,
    ) -> Result<Vec<dashboard::FastestChannel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .fastest_channels(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.fastestModels(input: FastestChannelsInput!): [FastestModel!]!`
    async fn fastest_models(
        &self,
        ctx: &Context<'_>,
        input: dashboard::FastestChannelsInput,
    ) -> Result<Vec<dashboard::FastestModel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .fastest_models(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.modelPerformanceStats: [ModelPerformanceStat!]!`
    async fn model_performance_stats(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<dashboard::ModelPerformanceStat>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .model_performance_stats()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.channelPerformanceStats: [ChannelPerformanceStat!]!`
    async fn channel_performance_stats(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<dashboard::ChannelPerformanceStat>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .channel_performance_stats()
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.tokenStatsByChannel(timeWindow: String): [TokenStatsByChannel!]!`
    async fn token_stats_by_channel(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::TokenStatsByChannel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .token_stats_by_channel(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.tokenStatsByModel(timeWindow: String): [TokenStatsByModel!]!`
    async fn token_stats_by_model(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::TokenStatsByModel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .token_stats_by_model(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.costStatsByChannel(timeWindow: String): [CostStatsByChannel!]!`
    async fn cost_stats_by_channel(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::CostStatsByChannel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .cost_stats_by_channel(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.costStatsByModel(timeWindow: String): [CostStatsByModel!]!`
    async fn cost_stats_by_model(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::CostStatsByModel>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .cost_stats_by_model(time_window)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Query.costStatsByAPIKey(timeWindow: String): [CostStatsByAPIKey!]!`
    #[graphql(name = "costStatsByAPIKey")]
    async fn cost_stats_by_api_key(
        &self,
        ctx: &Context<'_>,
        time_window: Option<String>,
    ) -> Result<Vec<dashboard::CostStatsByAPIKey>, String> {
        let services = dashboard::dashboard_services(ctx)?;
        services
            .cost_stats_by_api_key(time_window)
            .await
            .map_err(|err| err.to_string())
    }
}

/// Returns the admin schema builder with all non-root output types
/// registered (currently the Relay `Node` interface, which is only reachable
/// through `implements` clauses and therefore needs explicit registration).
/// Hosts chain `.data(...)` service wiring onto this before `.finish()`.
pub fn admin_schema_builder()
-> async_graphql::SchemaBuilder<QueryRoot, mutation::MutationRoot, EmptySubscription> {
    Schema::build(QueryRoot, mutation::MutationRoot, EmptySubscription)
        .register_output_type::<channel::Node>()
}

pub fn build_admin_schema() -> AdminSchema {
    admin_schema_builder().finish()
}

#[cfg(test)]
mod tests {
    use super::build_admin_schema;

    #[test]
    fn admin_schema_sdl_contains_placeholder_query_fields() {
        let sdl = build_admin_schema().sdl();

        assert!(sdl.contains("schema"));
        assert!(sdl.contains("query: QueryRoot"));
        assert!(sdl.contains("type QueryRoot"));
        assert!(sdl.contains("health: String!"));
        assert!(sdl.contains("version: String!"));
        assert!(sdl.contains("me: UserInfo!"));
    }

    #[test]
    fn frontend_p54_root_fields_are_present() {
        let sdl = build_admin_schema().sdl();
        for field in [
            "channelOverrideTemplates(",
            "createChannelOverrideTemplate(",
            "updateChannelOverrideTemplate(",
            "deleteChannelOverrideTemplate(",
            "applyChannelOverrideTemplate(",
            "clearChannelOverrideTemplates(",
            "channelProbeData(",
            "triggerGcCleanup(",
            "previewGcCleanup(",
            "clearCache(",
            "getCacheDiagnostics(",
            "backup(",
            "restore(",
            "triggerAutoBackup:",
        ] {
            assert!(
                sdl.contains(field),
                "admin SDL is missing frontend field {field}"
            );
        }
        assert!(
            !sdl.contains("modelsData"),
            "modelsData is a frontend variable, not a GraphQL root field"
        );
    }

    #[test]
    fn admin_schema_sdl_contains_enum_casing_skeleton() {
        let sdl = build_admin_schema().sdl();

        assert!(sdl.contains("enum QuotaEnforcementMode"));
        assert!(sdl.contains("EXHAUSTED_ONLY"));
        assert!(sdl.contains("DE_PRIORITIZE"));
        assert!(sdl.contains("enum AutoSyncFrequency"));
        assert!(sdl.contains("ONE_HOUR"));
        assert!(sdl.contains("SIX_HOURS"));
        // AutoSyncFrequency has no TWELVE_HOURS variant (Go parity):
        // the snapshot at tests/contracts/admin_graphql_schema.graphql only
        // declares ONE_HOUR / SIX_HOURS / ONE_DAY. See scalars.rs for the
        // canonical enum casing shim.
        assert!(sdl.contains("ONE_DAY"));
    }

    #[test]
    fn admin_schema_sdl_contains_connection_probe_shape() {
        let sdl = build_admin_schema().sdl();

        assert!(sdl.contains("connectionProbe: GraphqlConnectionProbe!"));
        assert!(sdl.contains("type GraphqlConnectionProbe"));
        assert!(sdl.contains("edges: [GraphqlConnectionProbeEdge!]!"));
        assert!(sdl.contains("pageInfo: PageInfo!"));
        assert!(sdl.contains("type GraphqlConnectionProbeEdge"));
        assert!(sdl.contains("cursor: String!"));
        assert!(sdl.contains("node: GraphqlConnectionProbeNode!"));
        assert!(sdl.contains("type PageInfo"));
        assert!(sdl.contains("hasNextPage: Boolean!"));
        assert!(sdl.contains("hasPreviousPage: Boolean!"));
        // Contract: PageInfo cursors are the `Cursor` scalar (snapshot
        // lines 4098/4102), not plain String.
        assert!(sdl.contains("startCursor: Cursor"));
        assert!(sdl.contains("endCursor: Cursor"));
    }
}

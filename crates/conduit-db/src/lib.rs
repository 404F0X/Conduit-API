#![forbid(unsafe_code)]

pub mod connection;
pub mod migrate;
pub mod pg_quota_admission;
pub mod policy;
pub mod pool;
#[cfg(test)]
pub(crate) mod postgres_test_support;
pub mod repo;
pub mod row;
pub mod tx;

pub use connection::{
    PostgresPools, connect_postgres, connect_postgres_pools, migrate_postgres,
    migrate_postgres_with_flag,
};
pub use migrate::{
    API_KEY_QUOTA_ADMISSIONS_SCHEMA_VERSION, BALANCE_SUBSCRIPTION_SCHEMA_VERSION,
    CHANNEL_QUOTA_SNAPSHOT_SCHEMA_VERSION, COMMERCIAL_OPERATION_AUDIT_SCHEMA_VERSION,
    COMMERCIALIZATION_SCHEMA_VERSION, Dialect, INITIAL_SCHEMA_VERSION, LATEST_SCHEMA_VERSION,
    MigrationOutcome, MigrationPlan, MigrationPlanError, MigrationPlanResult, MigrationRunnerError,
    MigrationStep, POSTGRES_INDEX_CLEANUP_SCHEMA_VERSION,
    POSTGRES_PERFORMANCE_INDEXES_SCHEMA_VERSION, POSTGRES_USAGE_INDEX_CLEANUP_SCHEMA_VERSION,
    PROJECT_COMMERCIALIZATION_SCHEMA_VERSION, PROJECT_WALLET_BALANCE_SNAPSHOTS_SCHEMA_VERSION,
    PROJECT_WALLET_SHADOW_LIFECYCLE_SCHEMA_VERSION, PROJECT_WALLET_SHADOW_SCHEMA_VERSION,
    PROVIDER_OBSERVATIONS_SCHEMA_VERSION, PROVIDER_QUOTA_PROBE_VERIFICATION_SCHEMA_VERSION,
    REQUEST_EXECUTION_CREDENTIAL_IDENTITY_SCHEMA_VERSION,
    REQUEST_ROUTE_EXPLANATIONS_SCHEMA_VERSION, ROUTE_AFFINITIES_SCHEMA_VERSION, RunnerPolicy,
    SCHEMA_MIGRATIONS_TABLE, SIMPLE_GROUP_MEMBERSHIP_SCHEMA_VERSION, SIMPLE_GROUP_SCHEMA_VERSION,
    SUBSCRIPTION_ASSIGNMENT_SNAPSHOTS_SCHEMA_VERSION, SUBSCRIPTION_ENTITLEMENTS_SCHEMA_VERSION,
    SUBSCRIPTION_PLAN_SNAPSHOTS_SCHEMA_VERSION, SUBSCRIPTION_PROJECT_GRANTS_SCHEMA_VERSION,
    USAGE_CHARGE_OUTBOX_SCHEMA_VERSION, initial_sql_for_dialect, run_migrations_postgres,
    select_dialect_entrypoint, validate_plan_non_destructive,
};
pub use policy::{
    PolicyContext, PolicyDecision, PolicyEntity, PolicyError, Principal, PrincipalKind,
    ProjectAccess, ProjectScope, WILDCARD_SCOPE, require_entity_access, require_principal,
    require_project_access, require_scope, scope_slug, with_bypass,
};
pub use pool::pool_options::{
    ResolvedPoolOptions, build_pool_options, default_resolved_pool_options, resolved_pool_options,
};
pub use pool::{
    DEFAULT_CONN_MAX_IDLE_TIME, DEFAULT_CONN_MAX_LIFETIME, DEFAULT_MAX_IDLE_CONNS,
    DEFAULT_MAX_OPEN_CONNS, DatabaseConfig, DbDialect, DbPoolHandle, DbRole, PoolRouter,
    PoolTarget, ReadRoute, ReplicaConfig, RouterError, SqlStatementKind, classify_sql,
};
pub use repo::api_key_repo::{
    CreateApiKeyInput, InMemoryApiKeyRepo, ListApiKeysQuery, ListApiKeysResult, UpdateApiKeyInput,
};
pub use repo::channel_model_price_repo::{
    ChannelModelPriceRepo, VERSION_STATUS_ACTIVE, VERSION_STATUS_ARCHIVED,
};
pub use repo::channel_repo::{
    CreateChannelInput, InMemoryChannelRepo, ListChannelsQuery, ListChannelsResult,
    UpdateChannelInput, cache_signature,
};
pub use repo::data_storage_repo::{
    CreateDataStorageInput, InMemoryDataStorageRepo, ListDataStoragesQuery, ListDataStoragesResult,
    UpdateDataStorageInput,
};
pub use repo::model_repo::{
    CreateModelInput, InMemoryModelRepo, ListModelsQuery, ListModelsResult, UpdateModelInput,
};
pub use repo::pg_api_key_repo::PgApiKeyRepo;
pub use repo::pg_channel_model_price_repo::PgChannelModelPriceRepo;
pub use repo::pg_channel_override_template_repo::PgChannelOverrideTemplateRepo;
pub use repo::pg_channel_repo::PgChannelRepo;
pub use repo::pg_data_storage_repo::PgDataStorageRepo;
pub use repo::pg_model_repo::PgModelRepo;
pub use repo::pg_oidc_repo::PgOidcRepo;
pub use repo::pg_profile_template_repo::PgProfileTemplateRepo;
pub use repo::pg_project_repo::PgProjectRepo;
pub use repo::pg_prompt_protection_repo::PgPromptProtectionRuleRepo;
pub use repo::pg_prompt_repo::PgPromptRepo;
pub use repo::pg_request_execution_repo::PgRequestExecutionRepo;
pub use repo::pg_request_repo::PgRequestRepo;
pub use repo::pg_role_repo::PgRoleRepo;
pub use repo::pg_route_affinity_repo::PgRouteAffinityRepo;
pub use repo::pg_system_repo::PgSystemRepo;
pub use repo::pg_thread_repo::PgThreadRepo;
pub use repo::pg_trace_repo::PgTraceRepo;
pub use repo::pg_usage_repo::PgUsageRepo;
pub use repo::pg_user_project_repo::PgUserProjectRepo;
pub use repo::pg_user_repo::PgUserRepo;
pub use repo::profile_template_repo::{
    CreateProfileTemplateInput, ProfileTemplateRepo, UpdateProfileTemplateInput,
};
pub use repo::project_repo::{
    CreateProjectInput, InMemoryProjectRepo, ListProjectsQuery, ListProjectsResult,
    UpdateProjectInput,
};
pub use repo::prompt_protection_repo::{
    CreateProtectionRuleInput, PromptProtectionRuleRepo, RULE_STATUS_ARCHIVED,
    RULE_STATUS_DISABLED, RULE_STATUS_ENABLED, UpdateProtectionRuleInput,
};
pub use repo::prompt_repo::{CreatePromptInput, InMemoryPromptRepo, UpdatePromptInput};
pub use repo::request_execution_repo::{
    CreateRequestExecutionInput, InMemoryRequestExecutionRepo, RequestExecutionRepo,
    UpdateRequestExecutionInput,
};
pub use repo::request_repo::{
    ContentSavedInput, CreateRequestInput, InMemoryRequestRepo, RequestListQuery,
    RequestListResult, UpdateRequestInput,
};
pub use repo::role_repo::{
    CreateRoleInput, InMemoryRoleRepo, ListRolesQuery, ListRolesResult, UpdateRoleInput,
};
pub use repo::route_affinity_repo::{
    InMemoryRouteAffinityRepo, KEY_CLASS_PREVIOUS_RESPONSE_ID, KEY_CLASS_PROMPT_CACHE_KEY,
    RouteAffinityKey, RouteAffinityRepo, UpsertRouteAffinityInput,
};
pub use repo::thread_repo::InMemoryThreadRepo;
pub use repo::trace_repo::InMemoryTraceRepo;
pub use repo::usage_repo::{
    CreateUsageLogInput, InMemoryUsageRepo, UsageListQuery, UsageListResult,
};
pub use repo::user_project_repo::{
    CreateUserProjectInput, InMemoryUserProjectRepo, UserProjectRepo,
};
pub use repo::user_repo::{
    CreateUserInput, InMemoryUserRepo, ListUsersQuery, ListUsersResult, UpdateUserInput,
};
pub use repo::{
    ApiKeyRepo, BackupRepo, ChannelRepo, DataStorageRepo, InMemoryBackupRepo, InMemoryOidcRepo,
    InMemoryProviderQuotaRepo, InMemorySystemRepo, ModelRepo, OidcRepo, ProjectRepo,
    PromptProtectionRepo, ProviderQuotaRepo, RepoError, RepoResult, RequestContext, RequestRepo,
    RoleRepo, SystemRepo, ThreadRepo, TraceRepo, UsageAggregate, UsageAggregateQuery, UsageRepo,
    UserRepo, guard_project_access, guard_repo_principal,
};
pub use row::{
    ApiKeyProfileTemplateRow, ApiKeyRow, BackupRow, ChannelModelPriceRow,
    ChannelModelPriceVersionRow, ChannelOverrideTemplateRow, ChannelProbeRow, ChannelRow,
    DataStorageRow, ModelRow, OidcIdentityRow, ProjectRow, PromptProtectionRuleRow, PromptRow,
    ProviderQuotaStatusRow, RequestExecutionRow, RequestRow, RoleRow, RouteAffinityRow, SystemRow,
    ThreadRow, TraceRow, UsageLogRow, UserProjectRow, UserRoleRow, UserRow,
};
pub use tx::{
    FakeTransactionManager, TransactionError, TransactionFuture, TransactionHandle,
    TransactionLogEntry, TransactionManager, TransactionState, run_in_tx, run_nested_in_tx,
};

//! Host-side service wiring — the integration layer the gateway binary owns.
//!
//! `conduit-http` depends only on the minimal handler-facing traits (e.g.
//! [`SystemService`]); this module supplies their concrete implementations,
//! built over the live PostgreSQL pool opened at boot, and returns a wired
//! [`AppServices`] ready for [`AppState`](conduit_http::AppState).
//!
//! This is the layer that turns the ported-but-unwired components into a
//! running gateway. Domains are added here as they land; the system domain
//! (`/admin/system/status`, `/initialize`, brand logo) is first because
//! `initialize` is the boot prerequisite that creates the owner user, default
//! project, and roles.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;

use conduit_admin_graphql::me::{SystemStatus, SystemStatusError};
// GAP-D system_ext channel/pass-through adapter — the two top-level types the
// adapter's method signatures reference. The frequency-enum <-> wire-literal
// conversions live in `domain_channel_to_graphql` / `graphql_channel_to_domain`
// with local `use` aliases (the domain frequency structs collide with the
// `channel_service` glob at the crate root, so they are imported by module
// path inside those functions).
use conduit_admin_graphql::system_ext::{
    SystemChannelError, SystemChannelSettings as GqlSystemChannelSettings,
};
// GAP-B model_ext adapter — the trait, its error surface, and the payload types
// the DB-backed subset returns. The remaining types (fetch/connection/price) are
// referenced by module path inside the adapter's stubbed methods.
use conduit_admin_graphql::model::ModelStatus as GqlModelStatus;
use conduit_admin_graphql::model_ext::{
    ModelExtError, ModelExtServices, ModelIdentityWithStatus as GqlModelIdentityWithStatus,
};
// P12-001 S07 base model CRUD adapter — the two host traits backing the
// `models` connection query + `createModel`/`updateModel`/`deleteModel`
// mutations, their shared error surface, and the connection arg/output types.
// Distinct from `ModelExtServices` above (which owns queryModels + the
// status/bulk mutations); the two traits partition the model domain with no
// method overlap.
use conduit_admin_graphql::channel::ChannelStatus as GqlChannelStatus;
use conduit_admin_graphql::model::{
    CreateModelInput as GqlCreateModelInput, Model as GqlModel, ModelAssociationCountServices,
    ModelConnection as GqlModelConnection, ModelConnectionArgs, ModelEdge as GqlModelEdge,
    ModelMutationServices, ModelOrderTerm, ModelQueryServices, ModelServiceError,
    ModelType as GqlModelType, ModelWhereInput as GqlModelWhereInput,
    UpdateModelInput as GqlUpdateModelInput,
};
// CONV-CH channel_queries adapter — the four channel list-page root queries.
// The trait + its lowered arg/output types; the DB-backed impl reuses Feng's
// `crate::conv::channel_row_to_gql` converter (CONV-01).
use conduit_admin_graphql::channel::{
    ChannelConnection as GqlChannelConnection, ChannelEdge as GqlChannelEdge,
    ChannelMutationServices, ChannelQueryServices,
};
use conduit_admin_graphql::channel_queries::{
    ChannelExtraQueryError, ChannelExtraQueryServices, ChannelSensitiveFields, ChannelTypeCount,
    CountChannelsByTypeArgs, QueryChannelsArgs,
};
// ChannelExt/Bulk mutation adapter (Wu) — status/duplicate/endpoints/bulk ops
// plus per-api-key enable/disable over the shared channel repository.
use conduit_admin_graphql::channel_ext::ChannelExtMutationServices;
use conduit_admin_graphql::channel_ext2::ChannelBulkMutationServices;
// Dashboard aggregation adapter (Feng) — cross-project GROUP BY/SUM stats
// issued directly over the shared pool, mirroring the Go resolvers.
use conduit_admin_graphql::dashboard::DashboardServices;
use conduit_admin_graphql::operations::OperationsServices;
// DataStorage adapter (Li) — unified marker trait with a blanket impl over the
// query+mutation traits, so a single .data() registration serves all three.
use conduit_admin_graphql::data_storage::DataStorageServices;
// Prompt domain adapters (Qin) — CRUD is database-backed;
// protection rules have no repo yet and surface ServiceUnavailable.
use conduit_admin_graphql::prompt::{
    PromptMutationServices, PromptProtectionRuleMutationServices,
    PromptProtectionRuleQueryServices, PromptQueryServices,
};
// RequestExecution query adapter (Han) — admin executions list over the new
// request-execution repository.
use conduit_admin_graphql::request_execution::RequestExecutionQueryServices;
// ProfileTemplate adapter (Wu) — template CRUD + loadTemplate-onto-api-key,
// over the profile-template repository + the shared API-key repository.
use conduit_admin_graphql::apikey::{ApiKeyMutationServices, ApiKeyQueryServices};
use conduit_admin_graphql::mutation::QuotaMutationServices;
use conduit_admin_graphql::product_experience::ProductExperienceServices;
use conduit_admin_graphql::profile_template::{
    ProfileTemplateMutationServices, ProfileTemplateQueryServices,
};
// SystemSettingsExt adapter (Li) — video storage / webhook notifier / auto
// backup settings + onboarding completion flags over the domain system service.
use conduit_admin_graphql::provider_quota_ext::ProviderQuotaStatusServices;
use conduit_admin_graphql::quota_ext::QuotaQueryServices;
use conduit_admin_graphql::request_usage::{RequestQueryServices, UsageLogQueryServices};
use conduit_admin_graphql::system_settings_ext::SystemSettingsExtServices;
// Node-id `TimeScalar` wrapper reused by the thread/trace row→GraphQL converters.
// GAP-E threads/traces GraphQL slice: the two host-query traits + their lowered
// arg/output types.
use conduit_admin_graphql::node::parse_guid;
use conduit_auth::{Claims, encode_hs256};
use conduit_cache::{Cache, NoopCache};
use conduit_core::error::ConduitError;
use conduit_core::objects::pricing as core_pricing;
use conduit_core::objects::user::UserInfo;
use conduit_core::{UpstreamErrorPolicy, UpstreamErrorPolicyMode};
use conduit_db::RouteAffinityKey;
use conduit_db::connection::{connect_postgres_pools, migrate_postgres_with_flag};
use conduit_db::repo::channel_model_price_repo::ChannelModelPriceRepo;
use conduit_db::repo::thread_repo::ThreadRepo;
use conduit_db::repo::trace_repo::TraceRepo;
use conduit_db::{
    ChannelRepo, CreateModelInput as RepoCreateModelInput, DatabaseConfig, DbDialect,
    ListChannelsQuery, ListModelsQuery, ModelRepo, PolicyContext, Principal, RequestContext,
    SystemRepo, UpdateModelInput,
};
use conduit_http::AppServices;
use conduit_http::auth_handlers::{
    AuthenticatedUser as HandlerAuthenticatedUser, SigninError, SigninService,
};
use conduit_http::middleware::api_key_auth::ApiKeyValidationService;
use conduit_http::openai_handlers::{
    ModelRow, ModelService as HttpModelService, OpenAiHandlerOutput, OpenAiOrchestratorService,
    OpenAiPricing, OpenAiRoute,
};
use conduit_http::system_handlers::{InitializeSystemParams, SystemService};
use conduit_llm::HttpClientBuilder;
use conduit_llm::model::HttpRequest as LlmHttpRequest;
use conduit_orchestrator::db_candidate_source::{DbCandidateSource, RoutingModelSettingsSource};
use conduit_orchestrator::load_balancer::{
    AdaptiveWeightedScoring, CostAwareScoring, RetryPolicy as LbRetryPolicy, ScoringStrategy,
    ScoringStrategySet, StaticStickyKeyProvider, WeightScoring,
};
use conduit_orchestrator::openai_bridge::{OpenAiOrchestratorBridge, OpenAiRoute as BridgeRoute};
use conduit_orchestrator::orchestrator::{
    CommandOrchestrator, DefaultCandidateProjector, FlagCancelToken,
    ROUTE_AFFINITY_API_FORMAT_METADATA, ROUTE_AFFINITY_DECISION_METADATA,
    ROUTE_AFFINITY_HINTS_METADATA, ROUTE_AFFINITY_KEY_CLASS_METADATA,
    ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA, ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA,
    ROUTE_AFFINITY_PUBLIC_MODEL_METADATA, RouteAffinityHint, RuntimeRetryPolicy,
    RuntimeRetryPolicySource, STICKY_CHANNEL_ID_METADATA,
};
use conduit_pipeline::pipeline::Pipeline;
use conduit_services::{
    AuthService, AuthServiceError, AuthUserRepo, AuthUserStatus, InitializeParams,
    SystemService as DomainSystemService, system_key,
};
use sqlx::PgPool;

fn build_outbound_transformer_registry()
-> Result<Arc<conduit_transformers::traits::TransformerRegistry>, String> {
    let mut registry = conduit_transformers::traits::TransformerRegistry::new();
    // The registry key is the candidate's target wire format. A channel may
    // therefore receive any supported inbound protocol after normalization to
    // the unified request model, without maintaining channel-type allowlists.
    registry.register_outbound_for_format(
        conduit_llm::ApiFormat::OpenAiChatCompletions,
        Arc::new(OpenAiCompatOutbound),
    );

    let anthropic: Arc<dyn conduit_transformers::OutboundTransformer> =
        Arc::new(conduit_transformers::AnthropicOutboundTransformer::new(
            conduit_transformers::AnthropicOutboundConfig {
                platform: conduit_transformers::anthropic::PlatformType::Direct,
                base_url: String::new(),
                api_key: String::new(),
                endpoint_path: Some("/v1/messages".to_string()),
                project_id: None,
                region: None,
            },
        ));
    registry.register_outbound_for_format(conduit_llm::ApiFormat::AnthropicMessages, anthropic);

    // Anthropic-compatible providers share the Messages wire format, but a
    // handful of them require platform-specific request shaping. Exact
    // channel registrations take precedence over the format fallback above.
    // In particular, Bedrock moves model/stream into the invocation URL and
    // uses its own version header; sending it the Direct payload is invalid.
    for (channel_type, platform) in [
        (
            "anthropic_aws",
            conduit_transformers::anthropic::PlatformType::Bedrock,
        ),
        (
            "longcat_anthropic",
            conduit_transformers::anthropic::PlatformType::LongCat,
        ),
        (
            "deepseek_anthropic",
            conduit_transformers::anthropic::PlatformType::DeepSeek,
        ),
        (
            "doubao_anthropic",
            conduit_transformers::anthropic::PlatformType::Doubao,
        ),
        (
            "moonshot_anthropic",
            conduit_transformers::anthropic::PlatformType::Moonshot,
        ),
        (
            "zhipu_anthropic",
            conduit_transformers::anthropic::PlatformType::Zhipu,
        ),
        (
            "zai_anthropic",
            conduit_transformers::anthropic::PlatformType::Zai,
        ),
    ] {
        registry.register_outbound(
            channel_type,
            conduit_llm::ApiFormat::AnthropicMessages,
            Arc::new(conduit_transformers::AnthropicOutboundTransformer::new(
                conduit_transformers::AnthropicOutboundConfig {
                    platform,
                    base_url: String::new(),
                    api_key: String::new(),
                    endpoint_path: Some("/v1/messages".to_string()),
                    project_id: None,
                    region: None,
                },
            )),
        );
    }

    // Keep the URL relative: the selected candidate owns the authoritative
    // base URL and credential, while the transformer owns the Gemini path.
    let gemini: Arc<dyn conduit_transformers::OutboundTransformer> = Arc::new(
        conduit_transformers::GeminiOutboundTransformer::with_config(
            conduit_transformers::gemini::GeminiOutboundConfig {
                base_url: String::new(),
                api_version: "v1beta".to_string(),
                endpoint_path: String::new(),
                platform_type: conduit_transformers::gemini::GeminiPlatformType::Direct,
            },
            String::new(),
        ),
    );
    registry.register_outbound_for_format(conduit_llm::ApiFormat::GeminiContents, gemini);

    let responses: Arc<dyn conduit_transformers::OutboundTransformer> = Arc::new(
        conduit_transformers::OpenAiResponsesOutbound::new("", "")
            .map_err(|error| format!("failed to initialize Responses transformer: {error}"))?,
    );
    registry.register_outbound_for_format(
        conduit_llm::ApiFormat::OpenAiResponses,
        Arc::clone(&responses),
    );
    registry
        .register_outbound_for_format(conduit_llm::ApiFormat::OpenAiResponsesCompact, responses);

    // These channel types need credentials/token refreshers or per-channel
    // cloud settings that are not available to this process-wide registry.
    // Register an exact fail-closed transformer for every chat wire format so
    // they cannot silently fall through to a Direct API-key transformer and
    // issue an unauthenticated or incorrectly authenticated request.
    for channel_type in [
        "anthropic_gcp",
        "gemini_vertex",
        "claudecode",
        "antigravity",
        "codex",
        "github_copilot",
    ] {
        for api_format in [
            conduit_llm::ApiFormat::OpenAiChatCompletions,
            conduit_llm::ApiFormat::AnthropicMessages,
            conduit_llm::ApiFormat::GeminiContents,
            conduit_llm::ApiFormat::OpenAiResponses,
            conduit_llm::ApiFormat::OpenAiResponsesCompact,
        ] {
            registry.register_outbound(
                channel_type,
                api_format,
                Arc::new(UnsupportedProviderOutbound { channel_type }),
            );
        }
    }
    Ok(Arc::new(registry))
}

struct UnsupportedProviderOutbound {
    channel_type: &'static str,
}

impl UnsupportedProviderOutbound {
    fn error(&self) -> ConduitError {
        ConduitError::internal(format!(
            "channel type {:?} requires a provider-specific outbound and authentication flow that is not wired into the Rust runtime",
            self.channel_type
        ))
    }
}

impl conduit_transformers::OutboundTransformer for UnsupportedProviderOutbound {
    fn name(&self) -> &'static str {
        "unsupported-provider-outbound"
    }

    fn outbound_request(
        &self,
        _request: &conduit_llm::LlmRequest,
    ) -> conduit_transformers::TransformerResult<LlmHttpRequest> {
        Err(self.error())
    }

    fn outbound_response(
        &self,
        _response: conduit_llm::HttpResponse,
    ) -> conduit_transformers::TransformerResult<conduit_llm::HttpResponse> {
        Err(self.error())
    }

    fn outbound_stream_event(
        &self,
        _event: conduit_llm::StreamEvent,
    ) -> conduit_transformers::TransformerResult<conduit_llm::StreamEvent> {
        Err(self.error())
    }

    fn outbound_error(
        &self,
        _response: conduit_llm::HttpResponse,
    ) -> conduit_transformers::TransformerResult<ConduitError> {
        Ok(self.error())
    }
}

#[cfg(test)]
mod outbound_transformer_registry_tests {
    use super::*;

    #[test]
    fn production_registry_resolves_every_supported_target_format() {
        let registry = build_outbound_transformer_registry()
            .expect("production transformer registry should initialize");
        for (format, expected_name) in [
            (
                conduit_llm::ApiFormat::OpenAiChatCompletions,
                "openai-compat-outbound",
            ),
            (conduit_llm::ApiFormat::AnthropicMessages, "anthropic"),
            (conduit_llm::ApiFormat::GeminiContents, "gemini"),
            (conduit_llm::ApiFormat::OpenAiResponses, "openai-responses"),
            (
                conduit_llm::ApiFormat::OpenAiResponsesCompact,
                "openai-responses",
            ),
        ] {
            let transformer = registry
                .outbound("custom-channel", format)
                .unwrap_or_else(|| {
                    panic!("missing production transformer for {}", format.as_str())
                });
            assert_eq!(transformer.name(), expected_name);
        }

        let responses = registry
            .outbound("custom-channel", conduit_llm::ApiFormat::OpenAiResponses)
            .expect("Responses transformer should resolve");
        let compact = registry
            .outbound(
                "custom-channel",
                conduit_llm::ApiFormat::OpenAiResponsesCompact,
            )
            .expect("Responses Compact transformer should resolve");
        assert!(Arc::ptr_eq(&responses, &compact));
    }

    #[test]
    fn production_registry_uses_bedrock_request_shape_for_anthropic_aws()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = build_outbound_transformer_registry()
            .expect("production transformer registry should initialize");
        let transformer = registry
            .outbound("anthropic_aws", conduit_llm::ApiFormat::AnthropicMessages)
            .ok_or("missing Bedrock transformer")?;
        let request = conduit_llm::LlmRequest {
            request_type: conduit_llm::RequestType::Chat,
            api_format: conduit_llm::ApiFormat::AnthropicMessages,
            model: Some("claude-bedrock".to_string()),
            stream: false,
            payload: conduit_llm::LlmRequestPayload::Chat(conduit_llm::ChatRequest {
                messages: vec![conduit_llm::ChatMessage {
                    role: "user".to_string(),
                    name: None,
                    content: Some(conduit_llm::MessageContent::Text("ping".to_string())),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    extra: Default::default(),
                }],
                ..Default::default()
            }),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };

        let http = transformer.outbound_request(&request)?;
        assert_eq!(http.path, "/model/claude-bedrock/invoke");
        assert_eq!(
            http.headers.get("Anthropic-Version").map(String::as_str),
            Some("bedrock-2023-05-31")
        );
        Ok(())
    }

    #[test]
    fn production_registry_fails_closed_for_unwired_provider_auth_flows() {
        let registry = build_outbound_transformer_registry()
            .expect("production transformer registry should initialize");
        for (channel_type, api_format) in [
            ("anthropic_gcp", conduit_llm::ApiFormat::AnthropicMessages),
            ("gemini_vertex", conduit_llm::ApiFormat::GeminiContents),
            ("claudecode", conduit_llm::ApiFormat::AnthropicMessages),
            ("antigravity", conduit_llm::ApiFormat::GeminiContents),
            ("codex", conduit_llm::ApiFormat::OpenAiResponses),
            (
                "github_copilot",
                conduit_llm::ApiFormat::OpenAiChatCompletions,
            ),
        ] {
            let transformer = registry
                .outbound(channel_type, api_format)
                .unwrap_or_else(|| panic!("missing fail-closed transformer for {channel_type}"));
            assert_eq!(transformer.name(), "unsupported-provider-outbound");
        }
    }
}

/// Build independent long-lived scorer instances for the three enterprise
/// load-balancer modes. The current runtime has channel weights available for
/// every mode; adaptive metrics and model-breaker state remain behind the
/// existing `ScoringStrategy` extension point and can be added without
/// changing per-request strategy resolution.
fn runtime_scoring_strategies(cost_score_weight: i64) -> ScoringStrategySet {
    let with_cost = |base: Arc<dyn ScoringStrategy>| -> Arc<dyn ScoringStrategy> {
        if cost_score_weight == 0 {
            base
        } else {
            Arc::new(CostAwareScoring::new(base, cost_score_weight))
        }
    };
    ScoringStrategySet::new(
        with_cost(Arc::new(AdaptiveWeightedScoring::new())),
        with_cost(Arc::new(WeightScoring::new())),
        with_cost(Arc::new(WeightScoring::new())),
    )
}

pub async fn build_runtime_services(
    config: &conduit_config::model::AppConfig,
) -> Result<
    (
        AppServices,
        conduit_db::PostgresPools,
        Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
    ),
    String,
> {
    let dialect = DbDialect::from_str(&config.db.dialect)
        .map_err(|e| format!("unsupported db.dialect {:?}: {e}", config.db.dialect))?;
    if dialect != DbDialect::Postgres {
        return Err(format!(
            "database dialect {dialect} is no longer supported; configure db.dialect=postgres"
        ));
    }
    build_postgres_core_services(config).await
}

async fn build_postgres_core_services(
    config: &conduit_config::model::AppConfig,
) -> Result<
    (
        AppServices,
        conduit_db::PostgresPools,
        Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
    ),
    String,
> {
    let mut db = DatabaseConfig::new(DbDialect::Postgres, &config.db.dsn);
    db.max_connections = config.db.max_open_conns;
    db.min_connections = config.db.max_idle_conns.min(config.db.max_open_conns);
    db.connect_timeout = config.db.connect_timeout;
    db.conn_max_lifetime = config.db.conn_max_lifetime;
    db.conn_max_idle_time = config.db.conn_max_idle_time;
    let read_replica = &config.db.read_replica;
    let mut pools = connect_postgres_pools(
        &db,
        Some(read_replica.read_dsn.as_str()),
        read_replica.read_max_open_conns,
        read_replica.read_max_idle_conns,
        read_replica.fallback_on_replica_failure,
    )
    .await
    .map_err(|e| format!("failed to open PostgreSQL pool: {e}"))?;
    let pool = pools.master_clone();
    migrate_postgres_with_flag(&pool, config.db.disable_auto_migration)
        .await
        .map_err(|e| format!("failed to run PostgreSQL migrations: {e}"))?;
    if pools.read().is_some() {
        let read_version = pools.read_schema_version().await;
        let read_is_current = matches!(
            &read_version,
            Ok(Some(version)) if version == conduit_db::LATEST_SCHEMA_VERSION
        );
        if !read_is_current && read_replica.fallback_on_replica_failure {
            pools.disable_read();
        } else if !read_is_current {
            return Err(match read_version {
                Ok(version) => format!(
                    "PostgreSQL read replica schema is not at {}; found {:?}",
                    conduit_db::LATEST_SCHEMA_VERSION,
                    version
                ),
                Err(error) => {
                    format!("failed to inspect PostgreSQL read replica schema: {error}")
                }
            });
        }
    }
    crate::wiring_model_catalog::ensure_upstream_model_deployments_postgres(&pool)
        .await
        .map_err(|e| format!("failed to initialize PostgreSQL channel model catalog: {e}"))?;
    let cache_config = cache_config_from_app(&config.cache);
    let requires_distributed = cache_mode_requires_distributed(cache_config.mode);
    let cache: Arc<dyn Cache> = match conduit_cache::build_cache(cache_config) {
        Ok(v) => v,
        Err(e) if requires_distributed => {
            return Err(format!(
                "cache.mode {:?} requires a working distributed backend: {e}",
                config.cache.mode
            ));
        }
        Err(e) => {
            tracing::warn!(%e,"failed to build cache backend; using no-op cache");
            Arc::new(NoopCache::new())
        }
    };
    let system_repo = Arc::new(conduit_db::PgSystemRepo::new(pool.clone()));
    let system = Arc::new(
        DomainSystemService::from_system_repo(system_repo.clone(), cache.clone())
            .with_repos(
                Arc::new(conduit_db::PgUserRepo::new(pool.clone())),
                Arc::new(conduit_db::PgProjectRepo::new(pool.clone())),
                Arc::new(conduit_db::PgRoleRepo::new(pool.clone())),
                Arc::new(conduit_db::PgUserProjectRepo::new(pool.clone())),
            )
            .with_data_storage_repo(Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone()))),
    );
    let auth_user_repo = Arc::new(crate::wiring_postgres_auth::PgAuthUserRepo::new(
        pool.clone(),
    ));
    let auth_key_repo = Arc::new(crate::wiring_postgres_auth::PgAuthApiKeyRepo::new(
        pool.clone(),
    ));
    let jwt_secret = if let Some(row) = system_repo
        .get_system_value_unchecked(&boot_request_context(), system_key::JWT_SECRET_KEY)
        .await
        .map_err(|e| e.to_string())?
    {
        decode_hex(&row.value).map_err(|e| format!("failed to decode JWT secret: {e:?}"))?
    } else if let Some(secret) = config.api_auth.jwt_secret.as_deref() {
        secret.as_bytes().to_vec()
    } else {
        conduit_auth::generate_secret_key().into_bytes()
    };
    let auth = Arc::new(
        AuthService::new(auth_user_repo.clone(), auth_key_repo, jwt_secret)
            .with_allow_no_auth(config.server.api.auth.allow_no_auth),
    );
    let signin = Arc::new(DbSigninService::new(
        auth.clone(),
        system_repo,
        config.api_auth.session_ttl,
    ));
    let signup = Arc::new(crate::wiring_postgres_auth::PgSignupService::new(
        pool.clone(),
        config.api_auth.bcrypt_cost,
    ));
    let model = Arc::new(DbModelService::new_with_repo(
        Arc::new(conduit_db::PgModelRepo::new(pool.clone())),
        pool.clone(),
    ));
    let api_key_validation: Arc<dyn ApiKeyValidationService> =
        Arc::new(crate::wiring_postgres_auth::PgApiKeyValidationService::new(
            auth,
            Arc::new(conduit_db::PgApiKeyRepo::new(pool.clone())),
            pool.clone(),
        ));
    let admin_model_repo: Arc<dyn ModelRepo> = Arc::new(conduit_db::PgModelRepo::new(pool.clone()));
    let admin_channel_repo: Arc<dyn ChannelRepo> =
        Arc::new(conduit_db::PgChannelRepo::new(pool.clone()));
    let admin_price_repo: Arc<dyn ChannelModelPriceRepo> =
        Arc::new(conduit_db::PgChannelModelPriceRepo::new(pool.clone()));
    let model_crud = Arc::new(ModelCrudAdapter::new(admin_model_repo.clone()));
    let channel_crud = Arc::new(crate::wiring_channel_crud::ChannelCrudAdapter::new(
        admin_channel_repo.clone(),
    ));
    let api_key_crud = Arc::new(crate::wiring_apikey::ApiKeyServiceAdapter::new(
        Arc::new(conduit_db::PgApiKeyRepo::new(pool.clone())),
        "conduit".into(),
    ));
    let profile_template_adapter = Arc::new(
        crate::wiring_profile_template::ProfileTemplateServiceAdapter::new(
            Arc::new(conduit_db::PgProfileTemplateRepo::new(pool.clone())),
            Arc::new(conduit_db::PgApiKeyRepo::new(pool.clone())),
        ),
    );
    let profile_template_query: Arc<dyn ProfileTemplateQueryServices> =
        profile_template_adapter.clone();
    let profile_template_mutation: Arc<dyn ProfileTemplateMutationServices> =
        profile_template_adapter;
    let channel_override_template: Arc<
        dyn conduit_admin_graphql::channel_override_template_ext::ChannelOverrideTemplateExtServices,
    > = Arc::new(
        crate::wiring_channel_override_template::ChannelOverrideTemplateAdapter::new(
            Arc::new(conduit_db::PgChannelOverrideTemplateRepo::new(pool.clone())),
            admin_channel_repo.clone(),
        ),
    );
    let prompt_adapter = Arc::new(crate::wiring_prompt::PromptCrudAdapter::new(Arc::new(
        conduit_db::PgPromptRepo::new(pool.clone()),
    )));
    let prompt_query: Arc<dyn PromptQueryServices> = prompt_adapter.clone();
    let prompt_mutation: Arc<dyn PromptMutationServices> = prompt_adapter;
    let prompt_rule_adapter = Arc::new(crate::wiring_prompt::PromptProtectionRuleAdapter::new(
        Arc::new(conduit_db::PgPromptProtectionRuleRepo::new(pool.clone())),
    ));
    let prompt_rule_query: Arc<dyn PromptProtectionRuleQueryServices> = prompt_rule_adapter.clone();
    let prompt_rule_mutation: Arc<dyn PromptProtectionRuleMutationServices> = prompt_rule_adapter;
    let request_query: Arc<dyn RequestQueryServices> =
        Arc::new(crate::wiring_requests::RequestAdapter::new(Arc::new(
            conduit_db::PgRequestRepo::new(pool.clone()),
        )));
    let usage_query: Arc<dyn UsageLogQueryServices> =
        Arc::new(crate::wiring_requests::UsageLogAdapter::new(Arc::new(
            conduit_db::PgUsageRepo::new(pool.clone()),
        )));
    let admin_data_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo> =
        Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone()));
    let data_storage: Arc<dyn DataStorageServices> = Arc::new(
        crate::wiring_data_storage::DataStorageAdapter::new(admin_data_storage_repo.clone()),
    );
    let backup_ext: Arc<dyn conduit_admin_graphql::backup_ext::BackupExtServices> =
        Arc::new(crate::wiring_postgres_backup::PgBackupExtAdapter::new(
            pool.clone(),
            system.clone(),
            admin_data_storage_repo.clone(),
        ));
    let execution_query: Arc<dyn RequestExecutionQueryServices> = Arc::new(
        crate::wiring_request_execution::RequestExecutionAdapter::new(Arc::new(
            conduit_db::PgRequestExecutionRepo::new(pool.clone()),
        ))
        .with_data_storage_repo(Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone()))),
    );
    let route_explanation: Arc<
        dyn conduit_admin_graphql::route_explanation::RouteExplanationServices,
    > = Arc::new(crate::wiring_route_explanation::RouteExplanationAdapter::postgres(pool.clone()));
    let content_request_repo: Arc<dyn conduit_db::repo::request_repo::RequestRepo> =
        Arc::new(conduit_db::PgRequestRepo::new(pool.clone()));
    let content_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo> =
        Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone()));
    let request_content: Arc<dyn conduit_http::RequestContentService> =
        Arc::new(crate::wiring_request_content::DbRequestContentService::new(
            content_request_repo.clone(),
            content_storage_repo.clone(),
        ));
    let request_preview: Arc<dyn conduit_http::RequestPreviewService> =
        Arc::new(crate::wiring_request_content::DbRequestPreviewService::new(
            content_request_repo,
            content_storage_repo,
        ));
    let oauth_admin: Arc<dyn conduit_http::OAuthAdminService> =
        Arc::new(crate::wiring_oauth_admin::OAuthAdminAdapter::new());
    let video_service: Arc<dyn conduit_http::VideoService> =
        Arc::new(crate::wiring_video::VideoAdapter::new(Arc::new(
            conduit_db::PgRequestRepo::new(pool.clone()),
        )));
    let me_query: Arc<dyn conduit_admin_graphql::me::MeServices> = Arc::new(
        crate::wiring_postgres_identity::PgMeServiceAdapter::new(pool.clone()),
    );
    let me_mutation: Arc<dyn conduit_admin_graphql::me_ext::MeMutationServices> = Arc::new(
        crate::wiring_postgres_identity::PgMeMutationAdapter::new(pool.clone()),
    );
    let channel_ext = Arc::new(crate::wiring_channel_ext::ChannelExtMutationAdapter::new(
        admin_channel_repo.clone(),
        pool.clone(),
    ));
    let channel_ext_mutation: Arc<dyn ChannelExtMutationServices> = channel_ext.clone();
    let channel_bulk_mutation: Arc<dyn ChannelBulkMutationServices> = channel_ext;
    let system_settings_ext: Arc<dyn SystemSettingsExtServices> = Arc::new(
        crate::wiring_system_settings_ext::SystemSettingsExtAdapter::new(
            system.clone(),
            admin_data_storage_repo,
        ),
    );
    let product_experience: Arc<dyn ProductExperienceServices> =
        Arc::new(crate::wiring_product_experience::ProductExperienceAdapter::new(system.clone()));
    let user_admin = Arc::new(
        crate::wiring_postgres_user::PostgresUserServiceAdapter::with_bcrypt_cost(
            pool.clone(),
            config.api_auth.bcrypt_cost,
        ),
    );
    let user_query: Arc<dyn conduit_admin_graphql::user::UserQueryServices> = user_admin.clone();
    let user_mutation: Arc<dyn conduit_admin_graphql::user::UserMutationServices> = user_admin;
    let billing: Arc<dyn conduit_admin_graphql::billing::BillingServices> = Arc::new(
        crate::wiring_postgres_billing::PgBillingAdapter::new(pool.clone()),
    );
    let project_repo_gql = Arc::new(conduit_db::PgProjectRepo::new(pool.clone()));
    let role_repo_gql = Arc::new(conduit_db::PgRoleRepo::new(pool.clone()));
    let project_admin = Arc::new(crate::wiring_postgres_project_role::PgProjectAdapter::new(
        project_repo_gql,
        role_repo_gql.clone(),
        pool.clone(),
    ));
    let project_query: Arc<dyn conduit_admin_graphql::project::ProjectQueryServices> =
        project_admin.clone();
    let project_mutation: Arc<dyn conduit_admin_graphql::project::ProjectMutationServices> =
        project_admin;
    let role_admin = Arc::new(crate::wiring_postgres_project_role::PgRoleAdapter::new(
        role_repo_gql,
        pool.clone(),
    ));
    let role_query: Arc<dyn conduit_admin_graphql::role::RoleQueryServices> = role_admin.clone();
    let role_mutation: Arc<dyn conduit_admin_graphql::role::RoleMutationServices> = role_admin;
    let model_market: Arc<dyn conduit_admin_graphql::model_catalog::ModelCatalogServices> =
        Arc::new(
            crate::wiring_postgres_model_market::PgModelMarketAdapter::new(
                pool.clone(),
                system.clone(),
            ),
        );
    let commercialization: Arc<
        dyn conduit_admin_graphql::commercialization::CommercializationServices,
    > = Arc::new(
        crate::wiring_postgres_commercialization::PgCommercializationAdapter::new(pool.clone()),
    );
    let simple_groups: Arc<dyn conduit_admin_graphql::simple_group::SimpleGroupServices> =
        Arc::new(crate::wiring_postgres_simple_group::PgSimpleGroupAdapter::new(pool.clone()));
    let dashboard: Arc<dyn DashboardServices> = Arc::new(
        crate::wiring_postgres_dashboard::PgDashboardAdapter::new(pool.clone())
            .with_read_pool(
                pools.read().cloned(),
                config.db.read_replica.fallback_on_replica_failure,
            )
            .with_offset(resolve_timezone_offset(&system).await),
    );
    let system_operations: Arc<
        dyn conduit_admin_graphql::system_operations_ext::SystemOperationsServices,
    > = Arc::new(
        crate::wiring_postgres_system_operations::PgSystemOperationsAdapter::new(
            pool.clone(),
            cache.clone(),
            system.clone(),
            conduit_services::GcConfig {
                cron: String::new(),
                vacuum_enabled: config.gc.vacuum_enabled,
                vacuum_full: config.gc.vacuum_full,
            },
        ),
    );
    let operations: Arc<dyn OperationsServices> = Arc::new(
        crate::wiring_postgres_operations::PgOperationsAdapter::new(pool.clone()).with_read_pool(
            pools.read().cloned(),
            config.db.read_replica.fallback_on_replica_failure,
        ),
    );
    let channel_probe_query: Arc<
        dyn conduit_admin_graphql::channel_probe_ext::ChannelProbeServices,
    > = Arc::new(
        crate::wiring_postgres_channel_probe_query::PgChannelProbeQueryAdapter::new(
            pool.clone(),
            system.clone(),
        ),
    );
    let provider_quota_adapter = Arc::new(
        crate::wiring_postgres_provider_quota::PgProviderQuotaAdapter::new(
            pool.clone(),
            system.clone(),
        ),
    );
    let quota_query: Arc<dyn QuotaQueryServices> = provider_quota_adapter.clone();
    let quota_mutation: Arc<dyn QuotaMutationServices> = provider_quota_adapter.clone();
    let provider_quota_status: Arc<dyn ProviderQuotaStatusServices> = provider_quota_adapter;
    let (thread_query, trace_query, node_resolver) =
        crate::wiring_postgres_observability::build_postgres_observability_services(pool.clone());
    let admin_schema = conduit_admin_graphql::admin_schema_builder()
        .extension(conduit_admin_graphql::authz_extension::ScopeAuthExtensionFactory)
        .data(Arc::new(SystemStatusAdapter {
            system: system.clone(),
        })
            as Arc<dyn conduit_admin_graphql::me::SystemStatusServices>)
        .data(Arc::new(SystemChannelAdapter {
            system: system.clone(),
        })
            as Arc<
                dyn conduit_admin_graphql::system_ext::SystemChannelServices,
            >)
        .data(Arc::new(SystemSettingsAdapter {
            system: system.clone(),
            pool: pool.clone(),
            http: reqwest::Client::new(),
            started_at: std::time::Instant::now(),
        })
            as Arc<
                dyn conduit_admin_graphql::system::SystemSettingsServices,
            >)
        .data(Arc::new(ModelExtAdapter::new(
            admin_model_repo.clone(),
            admin_channel_repo.clone(),
            system.clone(),
            pool.clone(),
        )) as Arc<dyn ModelExtServices>)
        .data(
            Arc::new(crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(
                pool.clone(),
            )) as Arc<dyn conduit_admin_graphql::change_set::ChangeSetServices>,
        )
        .data(model_crud.clone() as Arc<dyn ModelQueryServices>)
        .data(model_crud as Arc<dyn ModelMutationServices>)
        .data(Arc::new(ModelAssociationCountAdapter::new(
            admin_channel_repo.clone(),
            system.clone(),
        )) as Arc<dyn ModelAssociationCountServices>)
        .data(Arc::new(ChannelExtraQueryAdapter {
            channel_repo: admin_channel_repo.clone(),
            price_repo: admin_price_repo,
        }) as Arc<dyn ChannelExtraQueryServices>)
        .data(channel_crud.clone() as Arc<dyn ChannelQueryServices>)
        .data(channel_crud as Arc<dyn ChannelMutationServices>)
        .data(api_key_crud.clone() as Arc<dyn ApiKeyQueryServices>)
        .data(api_key_crud as Arc<dyn ApiKeyMutationServices>)
        .data(profile_template_query)
        .data(profile_template_mutation)
        .data(channel_override_template)
        .data(prompt_query)
        .data(prompt_mutation)
        .data(prompt_rule_query)
        .data(prompt_rule_mutation)
        .data(me_query)
        .data(me_mutation)
        .data(channel_ext_mutation)
        .data(channel_bulk_mutation)
        .data(system_settings_ext)
        .data(product_experience)
        .data(user_query)
        .data(user_mutation)
        .data(billing)
        .data(project_query)
        .data(project_mutation)
        .data(role_query)
        .data(role_mutation)
        .data(model_market)
        .data(commercialization)
        .data(simple_groups)
        .data(dashboard)
        .data(system_operations)
        .data(operations)
        .data(channel_probe_query)
        .data(quota_query)
        .data(quota_mutation)
        .data(provider_quota_status)
        .data(request_query)
        .data(usage_query)
        .data(data_storage)
        .data(backup_ext)
        .data(execution_query)
        .data(route_explanation)
        .data(thread_query)
        .data(trace_query)
        .data(node_resolver)
        .finish();
    let registry = Arc::new(conduit_orchestrator::live_streaming::LiveStreamRegistry::new());
    let openai_service = build_postgres_proxy_service(
        &pool,
        system.clone(),
        registry.clone(),
        cache.clone(),
        &config.cache.route_affinity,
        config.server.disable_ssl_verify,
    )
    .await?;
    let openapi_schema = conduit_openapi_graphql::build_openapi_schema(
        crate::wiring_postgres_openapi::build_postgres_openapi_services(
            pool.clone(),
            config.server.api.auth.key_prefix.clone(),
        ),
    );
    let oidc: Arc<dyn conduit_http::OidcService> =
        Arc::new(crate::wiring_oidc::OidcAdapter::new_postgres(
            config.oidc.clone(),
            config.api_auth.jwt_secret.clone(),
            config.api_auth.session_ttl,
            pool.clone(),
        ));
    let services = AppServices::new()
        .with_system_service(Arc::new(DbSystemService {
            system,
            pool: pool.clone(),
        }))
        .with_signin_service(signin)
        .with_signup_service(signup)
        .with_oidc_service(oidc)
        .with_request_content_service(request_content)
        .with_request_preview_service(request_preview)
        .with_oauth_admin_service(oauth_admin)
        .with_video_service(video_service)
        .with_model_service(model)
        .with_openai_orchestrator(openai_service)
        .with_api_key_validation_service(api_key_validation)
        .with_user_principal_service(Arc::new(DbJwtIdentityResolver {
            user_repo: auth_user_repo,
        }))
        .with_admin_schema(admin_schema)
        .with_openapi_schema(openapi_schema);
    Ok((services, pools, registry))
}

async fn build_postgres_proxy_service(
    pool: &PgPool,
    system: Arc<DomainSystemService>,
    live_registry: Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
    cache: Arc<dyn Cache>,
    route_affinity_config: &conduit_config::model::RouteAffinityConfig,
    insecure_skip_verify: bool,
) -> Result<Arc<dyn OpenAiOrchestratorService>, String> {
    let model_repo: Arc<dyn ModelRepo> = Arc::new(conduit_db::PgModelRepo::new(pool.clone()));
    let channel_repo: Arc<dyn ChannelRepo> = Arc::new(conduit_db::PgChannelRepo::new(pool.clone()));
    let quota_source: Arc<dyn conduit_orchestrator::db_candidate_source::QuotaAdmissionSource> =
        Arc::new(crate::wiring_postgres_quota::PgQuotaAdmissionSource::new(
            pool.clone(),
            system.clone(),
        ));
    let model_settings_source: Arc<dyn RoutingModelSettingsSource> =
        Arc::new(SystemRoutingModelSettingsSource {
            system: system.clone(),
        });
    let selected_candidate_source: Arc<dyn conduit_orchestrator::orchestrator::CandidateSource> =
        Arc::new(
            DbCandidateSource::new(channel_repo, model_repo)
                .with_quota_source(quota_source)
                .with_model_settings_source(model_settings_source),
        );
    let candidate_source: Arc<dyn conduit_orchestrator::orchestrator::CandidateSource> = Arc::new(
        crate::wiring_postgres_pricing_admission::PgPricingAdmissionCandidateSource::new(
            selected_candidate_source,
            pool.clone(),
        ),
    );
    let price_repo: Arc<dyn ChannelModelPriceRepo> =
        Arc::new(conduit_db::PgChannelModelPriceRepo::new(pool.clone()));
    let usage_repo: Arc<dyn conduit_db::repo::usage_repo::UsageRepo> =
        Arc::new(conduit_db::PgUsageRepo::new(pool.clone()));
    let route_affinity_runtime = Arc::new(crate::route_affinity::RouteAffinityRuntime::new(
        Arc::new(conduit_db::PgRouteAffinityRepo::new(pool.clone())),
        cache.clone(),
        crate::route_affinity::RouteAffinityRuntimeConfig::from(route_affinity_config),
    ));
    // Retention remains active when routing affinity is disabled so rows from
    // an earlier enabled period do not become permanent.
    crate::route_affinity::start_route_affinity_cleanup(route_affinity_runtime.clone());
    let route_affinity = route_affinity_config
        .enabled
        .then_some(route_affinity_runtime);
    let charge_settler =
        Arc::new(crate::usage_charge_settler_postgres::PgUsageChargeSettler::new(pool.clone()));
    crate::usage_charge_settler_postgres::start_reconciler(charge_settler.clone());
    let recorder = Arc::new(
        crate::usage_log_recorder::UsageLogRecorder::new(usage_repo)
            .with_sticky_channel_cache(cache.clone())
            .with_route_affinity_runtime(route_affinity.clone())
            .with_price_repo(price_repo)
            .with_charge_settler(charge_settler)
            .with_postgres_stream_persistence(pool.clone()),
    );
    let request_repo: Arc<dyn conduit_db::repo::request_repo::RequestRepo> =
        Arc::new(conduit_db::PgRequestRepo::new(pool.clone()));
    let execution_repo: Arc<dyn conduit_db::repo::request_execution_repo::RequestExecutionRepo> =
        Arc::new(conduit_db::PgRequestExecutionRepo::new(pool.clone()));
    let thread_repo: Arc<dyn ThreadRepo> = Arc::new(conduit_db::PgThreadRepo::new(pool.clone()));
    let trace_repo: Arc<dyn TraceRepo> = Arc::new(conduit_db::PgTraceRepo::new(pool.clone()));
    let request_artifact_storage: Arc<
        dyn conduit_orchestrator::middlewares::persist::RequestArtifactStorage,
    > = Arc::new(
        crate::wiring_request_content::DbRequestArtifactStorage::new(
            system.clone(),
            Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone())),
        ),
    );
    let prompt_inject_source = Arc::new(PromptRepoSource {
        repo: Arc::new(conduit_db::PgPromptRepo::new(pool.clone())),
    });
    let prompt_inject_middleware: conduit_pipeline::BoxPipelineMiddleware = Arc::new(
        conduit_orchestrator::middlewares::InjectPromptsMiddleware::new(prompt_inject_source),
    );
    let prompt_protection_repo: Arc<
        dyn conduit_db::repo::prompt_protection_repo::PromptProtectionRuleRepo,
    > = Arc::new(conduit_db::PgPromptProtectionRuleRepo::new(pool.clone()));
    let prompt_protection_middleware: conduit_pipeline::BoxPipelineMiddleware = Arc::new(
        conduit_orchestrator::middlewares::PromptProtectionMiddleware::new(prompt_protection_repo),
    );
    let inbound: Arc<dyn conduit_transformers::InboundTransformer> =
        Arc::new(conduit_transformers::OpenAiChatInbound::new());
    let outbound: Arc<dyn conduit_transformers::OutboundTransformer> =
        Arc::new(OpenAiCompatOutbound);
    let client = HttpClientBuilder::new()
        .insecure_skip_verify(insecure_skip_verify)
        .build()
        .map_err(|e| format!("failed to build PostgreSQL runtime upstream client: {e}"))?;
    let executor: Arc<dyn conduit_pipeline::Executor> = Arc::new(
        conduit_orchestrator::upstream_executor::UpstreamExecutor::new(client)
            .with_insecure_skip_verify(insecure_skip_verify),
    );
    let runtime_retry_policy = resolve_runtime_retry_policy(&system).await;
    let retry_policy_source: Arc<dyn RuntimeRetryPolicySource> =
        Arc::new(SystemRuntimeRetryPolicySource::new(system.clone()));
    let attempt_observer =
        crate::auto_disable_runtime::start_auto_disable_runtime(pool.clone(), system.clone());
    let pipeline = Arc::new(
        Pipeline::new(inbound, outbound, executor)
            .with_retry_hooks(conduit_pipeline::RetryHooks {
                can_retry: conduit_pipeline::channel_retry_hook(None),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(conduit_pipeline::is_response_timeout_error),
            })
            .with_retry_policy(runtime_retry_policy.pipeline)
            .with_middlewares(vec![
                Arc::new(conduit_orchestrator::middlewares::StripBillingHeaderMiddleware),
                Arc::new(conduit_orchestrator::middlewares::EnsureUsageMiddleware),
                Arc::new(conduit_orchestrator::middlewares::QuotaEnforcementMiddleware::new()),
                Arc::new(
                    conduit_orchestrator::middlewares::AutoReasoningEffortMiddleware::new(
                        Arc::new(AutoReasoningModelSettingsSource {
                            system: system.clone(),
                        }),
                    ),
                ),
                Arc::new(conduit_orchestrator::middlewares::CheckModelAccessMiddleware),
                Arc::new(conduit_orchestrator::middlewares::ModelMappingMiddleware),
                prompt_inject_middleware,
                prompt_protection_middleware,
                Arc::new(conduit_orchestrator::middlewares::PassThroughMiddleware),
                Arc::new(conduit_orchestrator::middlewares::PassThroughResponseMiddleware),
                Arc::new(
                    conduit_orchestrator::middlewares::PersistRequestMiddleware::new(
                        request_repo.clone(),
                    )
                    .with_artifact_storage(request_artifact_storage.clone()),
                ),
                Arc::new(conduit_orchestrator::middlewares::OverrideRequestMiddleware),
                Arc::new(conduit_orchestrator::middlewares::PassThroughRequestBodyMiddleware),
                Arc::new(conduit_orchestrator::middlewares::DefaultUserAgentMiddleware),
                Arc::new(conduit_orchestrator::middlewares::PerformanceRecordingMiddleware),
                Arc::new(
                    conduit_orchestrator::middlewares::ModelCircuitBreakerMiddleware::new(
                        conduit_orchestrator::middlewares::circuit_breaker::CircuitBreakerConfig {
                            max_errors: 5,
                            cooldown_duration: std::time::Duration::from_secs(60),
                        },
                    ),
                ),
                Arc::new(
                    conduit_orchestrator::middlewares::PersistRequestExecutionMiddleware::new(
                        execution_repo,
                    )
                    .with_artifact_storage(request_artifact_storage.clone()),
                ),
                Arc::new(conduit_orchestrator::middlewares::ChannelConcurrencyMiddleware::new(0)),
                Arc::new(conduit_orchestrator::middlewares::RateLimitAdmissionMiddleware::new()),
                Arc::new(conduit_orchestrator::middlewares::RateLimitTrackingMiddleware),
                Arc::new(conduit_orchestrator::middlewares::CaptureRawProviderMiddleware),
                Arc::new(conduit_orchestrator::middlewares::CaptureStreamMiddleware::new()),
                Arc::new(
                    conduit_orchestrator::middlewares::PassThroughStreamMiddleware::new(Arc::new(
                        std::sync::Mutex::new(Vec::new()),
                    )),
                ),
                Arc::new(
                    conduit_orchestrator::middlewares::LivePreviewMiddleware::new(live_registry),
                ),
            ])
            .with_attempt_observer(attempt_observer)
            .with_outbound_registry(build_outbound_transformer_registry()?),
    );
    let orchestrator = Arc::new(
        CommandOrchestrator::new(
            candidate_source,
            Arc::new(DefaultCandidateProjector),
            Arc::new(WeightScoring::new()),
            Arc::new(StaticStickyKeyProvider::none()),
            runtime_retry_policy.load_balancer,
            pipeline,
            recorder,
            Arc::new(FlagCancelToken::new()),
        )
        .with_scoring_strategies(runtime_scoring_strategies(0))
        .with_runtime_retry_policy_source(retry_policy_source)
        .with_route_health_source(Arc::new(
            crate::wiring_route_health::PgRouteHealthSource::new(pool.clone()),
        )),
    );
    Ok(Arc::new(BridgeOrchestratorService {
        bridge: Arc::new(OpenAiOrchestratorBridge::new(orchestrator)),
        system,
        request_repo,
        thread_repo,
        trace_repo,
        cache,
        route_affinity,
        request_artifact_storage,
    }))
}

/// Sync bridge adapting an async prompt repository to the middleware's
/// [`PromptSource`] trait (prompt injection middleware needs a sync call).
struct PromptRepoSource {
    repo: Arc<dyn conduit_db::repo::prompt_repo::PromptRepo>,
}

impl conduit_orchestrator::middlewares::prompt_injection::PromptSource for PromptRepoSource {
    fn list_enabled_prompts(
        &self,
        project_id: &str,
    ) -> Result<Vec<conduit_db::row::PromptRow>, String> {
        let ctx = conduit_db::RequestContext::new(conduit_db::PolicyContext::new(
            conduit_db::Principal::system(),
        ));
        let handle = tokio::runtime::Handle::current();
        let pid = project_id.to_string();
        let rows = tokio::task::block_in_place(|| {
            handle
                .block_on(self.repo.list_prompts_unchecked(&ctx, &pid))
                .map_err(|e| e.to_string())
        })?;
        // Filter to enabled prompts only (Go: svc.ListEnabledRules).
        Ok(rows.into_iter().filter(|r| r.status == "enabled").collect())
    }
}

#[cfg(test)]
mod postgres_prompt_runtime_tests {
    use super::*;
    use conduit_db::repo::prompt_protection_repo::{
        CreateProtectionRuleInput, PromptProtectionRuleRepo, RULE_STATUS_ENABLED,
    };
    use conduit_db::repo::prompt_repo::{CreatePromptInput, PromptRepo};
    use conduit_llm::{
        ApiFormat, ChatMessage, ChatRequest, LlmRequest, LlmRequestPayload, MessageContent,
        RequestType,
    };
    use conduit_orchestrator::middlewares::prompt_injection::PromptSource;
    use conduit_pipeline::PipelineMiddleware;

    fn prompt_input(project_id: &str, name: &str, status: Option<&str>) -> CreatePromptInput {
        CreatePromptInput {
            id: "ignored".into(),
            project_id: project_id.into(),
            name: name.into(),
            description: None,
            role: "system".into(),
            content: format!("injected-{name}"),
            status: status.map(str::to_owned),
            order: Some(0),
            settings: Some(serde_json::json!({"action": {"type": "prepend"}})),
            created_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    fn chat_request(text: &str) -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-test".into()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages: vec![ChatMessage {
                    role: "user".into(),
                    content: Some(MessageContent::Text(text.into())),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    name: None,
                    extra: Default::default(),
                }],
                ..Default::default()
            }),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres_runtime_injects_only_project_enabled_prompts_and_applies_protection()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let ctx = conduit_db::RequestContext::new(conduit_db::PolicyContext::new(
            conduit_db::Principal::system(),
        ));
        let prompt_repo: Arc<dyn PromptRepo> =
            Arc::new(conduit_db::PgPromptRepo::new(database.pool.clone()));
        prompt_repo
            .create_prompt_unchecked(&ctx, prompt_input("1", "enabled", Some("enabled")))
            .await?;
        prompt_repo
            .create_prompt_unchecked(&ctx, prompt_input("1", "disabled", None))
            .await?;
        prompt_repo
            .create_prompt_unchecked(&ctx, prompt_input("2", "other-project", Some("enabled")))
            .await?;

        let source = Arc::new(PromptRepoSource {
            repo: prompt_repo.clone(),
        });
        let enabled = source.list_enabled_prompts("1")?;
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "enabled");

        let inject = conduit_orchestrator::middlewares::InjectPromptsMiddleware::new(source);
        let mut pipeline_ctx = conduit_pipeline::middleware::PipelineContext::new();
        pipeline_ctx
            .metadata
            .insert("project_id".into(), "1".into());
        let injected = inject.on_inbound_llm_request(
            &mut pipeline_ctx,
            chat_request("the secret-123 must be hidden"),
        )?;
        let injected_messages = match &injected.payload {
            LlmRequestPayload::Chat(chat) => &chat.messages,
            _ => return Err("expected chat request".into()),
        };
        assert_eq!(injected_messages.len(), 2);
        assert_eq!(injected_messages[0].role, "system");
        assert_eq!(
            injected_messages[0].content,
            Some(MessageContent::Text("injected-enabled".into()))
        );

        let protection_repo: Arc<dyn PromptProtectionRuleRepo> = Arc::new(
            conduit_db::PgPromptProtectionRuleRepo::new(database.pool.clone()),
        );
        let rule = protection_repo
            .create_protection_rule_unchecked(
                &ctx,
                CreateProtectionRuleInput {
                    name: "mask-secrets".into(),
                    description: None,
                    pattern: "secret-[0-9]+".into(),
                    settings: serde_json::json!({
                        "action": "mask",
                        "replacement": "[MASKED]",
                        "scopes": ["user"]
                    }),
                    created_at: "2024-01-01T00:00:00Z".into(),
                },
            )
            .await?;
        protection_repo
            .set_protection_rule_status_unchecked(
                &ctx,
                &rule.id,
                RULE_STATUS_ENABLED,
                "2024-01-02T00:00:00Z".into(),
            )
            .await?;
        let protect =
            conduit_orchestrator::middlewares::PromptProtectionMiddleware::new(protection_repo);
        let protected = protect.on_inbound_llm_request(&mut pipeline_ctx, injected)?;
        let protected_messages = match &protected.payload {
            LlmRequestPayload::Chat(chat) => &chat.messages,
            _ => return Err("expected chat request".into()),
        };
        assert_eq!(
            protected_messages[1].content,
            Some(MessageContent::Text("the [MASKED] must be hidden".into()))
        );

        database.cleanup().await?;
        Ok(())
    }
}

/// Sync bridge for the auto-reasoning-effort middleware: reads
/// `model_settings.auto_reasoning_effort` from the system service.
struct AutoReasoningModelSettingsSource {
    system: Arc<DomainSystemService>,
}

impl conduit_orchestrator::middlewares::auto_reasoning::ModelSettingsSource
    for AutoReasoningModelSettingsSource
{
    fn auto_reasoning_effort_enabled(&self) -> bool {
        let handle = tokio::runtime::Handle::current();
        let ctx = boot_request_context();
        tokio::task::block_in_place(|| {
            match handle.block_on(self.system.model_settings(&ctx)) {
                Ok(settings) => settings.auto_reasoning_effort,
                Err(err) => {
                    // A read failure here silently disabled the feature (P-49).
                    // Log it so an admin who enabled model-high splitting can
                    // tell it degraded, rather than the setting appearing to be
                    // intermittently ignored. Fall back to disabled (safe).
                    tracing::warn!(
                        %err,
                        "model_settings read failed; auto_reasoning_effort disabled for this request"
                    );
                    false
                }
            }
        })
    }
}

/// Async production source for routing-relevant system model settings.
struct SystemRoutingModelSettingsSource {
    system: Arc<DomainSystemService>,
}

#[async_trait]
impl RoutingModelSettingsSource for SystemRoutingModelSettingsSource {
    async fn current(&self) -> conduit_core::objects::SystemModelSettings {
        let ctx = boot_request_context();
        match self.system.model_settings(&ctx).await {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(
                    %err,
                    "model_settings read failed; using routing defaults for this request"
                );
                conduit_core::objects::SystemModelSettings::default()
            }
        }
    }
}

/// Handler-facing [`SystemService`] backed by the domain
/// `conduit_services::SystemService` over PostgreSQL repositories. The host owns this
/// bridge so `conduit-http` stays decoupled from `conduit-services`/`conduit-db`.
struct DbSystemService {
    system: Arc<DomainSystemService>,
    pool: PgPool,
}

/// GraphQL-facing [`SystemStatusServices`] adapter backed by the same domain
/// `SystemService`. Lets the admin schema's `systemStatus` resolver reach the
/// live DB (via `is_initialized`) instead of returning "service unavailable".
struct SystemStatusAdapter {
    system: Arc<DomainSystemService>,
}

#[async_trait]
impl conduit_admin_graphql::me::SystemStatusServices for SystemStatusAdapter {
    async fn system_status(&self) -> Result<SystemStatus, SystemStatusError> {
        let ctx = boot_request_context();
        let is_initialized = self
            .system
            .is_initialized(&ctx)
            .await
            .map_err(|err| SystemStatusError::InitializationStatus(err.to_string()))?;
        Ok(SystemStatus { is_initialized })
    }
}

// ---------------------------------------------------------------------------
// SystemChannel adapter — backs the admin GraphQL `systemChannelSettings` /
// `passThroughSettings` queries + their update mutations (GAP-D system_ext
// slice). Delegates to the same domain `SystemService` the other adapters use.
// ---------------------------------------------------------------------------

/// GraphQL-facing [`SystemChannelServices`] adapter backed by the domain
/// `SystemService`. Mirrors Go `system.resolvers.go`, whose channel-settings /
/// pass-through resolvers all delegate to `systemService.{ChannelSetting,
/// ChannelSettingOrDefault,SetChannelSetting,PassThrough,SetPassThrough}`.
struct SystemChannelAdapter {
    system: Arc<DomainSystemService>,
}

/// Translate the config-crate `CacheConfig` (string mode + rich redis fields)
/// into the cache-crate `CacheConfig` the `build_cache` factory consumes.
///
/// Mirrors Go `xcache.NewFromConfig` (`internal/pkg/xcache/cache.go:68`): an
/// empty/unknown mode degrades to the no-op cache. A recognized distributed
/// mode still fails startup when its backend cannot be built. `redis.cluster`
/// takes precedence over `redis.sentinel` when both are set (matching the Go
/// setter-cache builder, which checks cluster first).
fn cache_config_from_app(
    config: &conduit_config::model::CacheConfig,
) -> conduit_cache::CacheConfig {
    let mode =
        conduit_cache::CacheMode::from_name(&config.mode).unwrap_or(conduit_cache::CacheMode::Noop);
    let redis_mode = if config.redis.cluster {
        conduit_cache::RedisMode::Cluster
    } else if config.redis.sentinel {
        conduit_cache::RedisMode::Sentinel
    } else {
        conduit_cache::RedisMode::Standalone
    };
    conduit_cache::CacheConfig {
        mode,
        memory: conduit_cache::MemoryCacheConfig {
            default_ttl: config.memory.ttl,
        },
        redis: conduit_cache::RedisConnectionConfig {
            url: config.redis.url.clone(),
            addr: config.redis.addr.clone(),
            username: config.redis.username.clone(),
            password: config.redis.password.clone(),
            db: config.redis.db,
            tls: config.redis.tls,
            mode: redis_mode,
            addrs: config.redis.addrs.clone(),
            master_name: config.redis.master_name.clone(),
        },
        key_prefix: None,
    }
}

fn cache_mode_requires_distributed(mode: conduit_cache::CacheMode) -> bool {
    matches!(
        mode,
        conduit_cache::CacheMode::Redis | conduit_cache::CacheMode::TwoLevel
    )
}

#[cfg(test)]
mod cache_runtime_wiring_tests {
    use super::*;

    #[test]
    fn every_distributed_cache_alias_is_fail_fast_at_startup() {
        for name in ["redis", "two-level", "two_level", "twolevel", "tiered"] {
            let mode = conduit_cache::CacheMode::from_name(name).expect("known cache mode");
            assert!(cache_mode_requires_distributed(mode), "mode {name}");
        }

        for name in ["noop", "memory"] {
            let mode = conduit_cache::CacheMode::from_name(name).expect("known cache mode");
            assert!(!cache_mode_requires_distributed(mode), "mode {name}");
        }
    }
}

/// Map the domain `SystemChannelSettings` (string-newtype frequencies) into the
/// GraphQL `SystemChannelSettings` (typed enums). Unknown/legacy frequency
/// strings fall back to the same defaults the Go `AutoSyncFrequency` /
/// `ProbeFrequency` marshalers use (auto-sync → `ONE_HOUR`; probe → the
/// contract default `OneMinute` for any non-canonical value — the probe wire
/// literals are `1m`/`5m`/`30m`/`1h`).
fn domain_channel_to_graphql(
    settings: conduit_services::SystemChannelSettings,
) -> GqlSystemChannelSettings {
    use conduit_admin_graphql::AutoSyncFrequency as GqlAuto;
    use conduit_admin_graphql::system_ext::{
        ChannelModelAutoSyncSetting as GqlAutoSync, ChannelProbeSetting as GqlProbe, ProbeFrequency,
    };

    // auto-sync: domain stores canonical `1h`/`6h`/`1d` (normalized on read).
    // Match on the raw wire literals (the `system_service` enum consts) rather
    // than `conduit_services::AutoSyncFrequency::*`, which is an ambiguous glob.
    let auto_sync_frequency = match settings.auto_sync.frequency.0.as_str() {
        conduit_services::system_service::AutoSyncFrequency::SIX_HOURS => GqlAuto::SixHours,
        conduit_services::system_service::AutoSyncFrequency::ONE_DAY => GqlAuto::OneDay,
        // `1h` and any unexpected value → ONE_HOUR (Go default).
        _ => GqlAuto::OneHour,
    };

    // probe frequency: domain wire literals `1m`/`5m`/`30m`/`1h`.
    let probe_frequency = match settings.probe.frequency.0.as_str() {
        "5m" => ProbeFrequency::FiveMinutes,
        "30m" => ProbeFrequency::ThirtyMinutes,
        "1h" => ProbeFrequency::OneHour,
        // `1m` and any unexpected value → the enum default OneMinute.
        _ => ProbeFrequency::OneMinute,
    };

    GqlSystemChannelSettings {
        probe: GqlProbe {
            enabled: settings.probe.enabled,
            frequency: probe_frequency,
        },
        auto_sync: GqlAutoSync {
            frequency: auto_sync_frequency,
        },
    }
}

/// Map the GraphQL `SystemChannelSettings` (typed enums) back into the domain
/// struct (string-newtype frequencies) for persistence. The frequency strings
/// use the canonical wire literals the domain getter normalizes against.
fn graphql_channel_to_domain(
    settings: GqlSystemChannelSettings,
) -> conduit_services::SystemChannelSettings {
    use conduit_admin_graphql::AutoSyncFrequency as GqlAuto;
    use conduit_admin_graphql::system_ext::ProbeFrequency;
    // `system_service`'s probe types are named `System*` to avoid colliding
    // with `channel_service`'s stricter enum versions of the same concept
    // (P-28): the system-settings side is a lenient wire passthrough, the
    // channel side a 4-value enum. Keeping them distinct types preserves both
    // contracts.
    use conduit_services::system_service::{
        AutoSyncFrequency as DomAuto, ChannelModelAutoSyncSetting as DomAutoSync,
        SystemChannelProbeSetting as DomProbe, SystemChannelSettings as DomSettings,
        SystemProbeFrequency as DomProbeFreq,
    };

    let auto_sync_wire = match settings.auto_sync.frequency {
        GqlAuto::OneHour => DomAuto::ONE_HOUR,
        GqlAuto::SixHours => DomAuto::SIX_HOURS,
        GqlAuto::OneDay => DomAuto::ONE_DAY,
    };
    let probe_wire = match settings.probe.frequency {
        ProbeFrequency::OneMinute => "1m",
        ProbeFrequency::FiveMinutes => "5m",
        ProbeFrequency::ThirtyMinutes => "30m",
        ProbeFrequency::OneHour => "1h",
    };

    DomSettings {
        probe: DomProbe {
            enabled: settings.probe.enabled,
            frequency: DomProbeFreq(probe_wire.to_string()),
        },
        auto_sync: DomAutoSync {
            frequency: DomAuto(auto_sync_wire.to_string()),
        },
        // A fresh set from the GraphQL layer carries no legacy/unknown fields.
        extra: std::collections::BTreeMap::new(),
    }
}

#[async_trait]
impl conduit_admin_graphql::system_ext::SystemChannelServices for SystemChannelAdapter {
    async fn channel_setting(&self) -> Result<GqlSystemChannelSettings, SystemChannelError> {
        let ctx = boot_request_context();
        let settings = self
            .system
            .channel_setting(&ctx)
            .await
            .map_err(|err| SystemChannelError::ChannelSetting(err.to_string()))?;
        Ok(domain_channel_to_graphql(settings))
    }

    async fn channel_setting_or_default(
        &self,
    ) -> Result<GqlSystemChannelSettings, SystemChannelError> {
        // Mirrors Go `ChannelSettingOrDefault`: never errors — a read failure
        // logs and falls back to the default. The domain method already returns
        // the default on a missing key; we additionally map any hard error to
        // the default so the merge-then-write mutation path stays resilient.
        let ctx = boot_request_context();
        // Domain `channel_setting_or_default` mirrors Go: it never errors, it
        // returns the default on any read failure (returns the value directly,
        // not a Result).
        let settings = self.system.channel_setting_or_default(&ctx).await;
        Ok(domain_channel_to_graphql(settings))
    }

    async fn set_channel_setting(
        &self,
        settings: GqlSystemChannelSettings,
    ) -> Result<(), SystemChannelError> {
        let ctx = boot_request_context();
        self.system
            .set_channel_setting(&ctx, graphql_channel_to_domain(settings))
            .await
            .map_err(|err| SystemChannelError::UpdateChannelSetting(err.to_string()))?;
        Ok(())
    }

    async fn pass_through(&self) -> Result<bool, SystemChannelError> {
        let ctx = boot_request_context();
        self.system
            .pass_through(&ctx)
            .await
            .map_err(|err| SystemChannelError::PassThrough(err.to_string()))
    }

    async fn set_pass_through(&self, enabled: bool) -> Result<(), SystemChannelError> {
        let ctx = boot_request_context();
        self.system
            .set_pass_through(&ctx, enabled)
            .await
            .map_err(|err| SystemChannelError::UpdatePassThrough(err.to_string()))
    }
}

// ---------------------------------------------------------------------------
// SystemSettings adapter (SVC-01) — backs the admin GraphQL system-settings
// slice: version / update-check / brand / proxy presets / security /
// onboarding / storage policy / retry policy / user-agent pass-through /
// default data storage / general settings / model settings. Mirrors the Go
// `system.resolvers.go` branches, all of which delegate to `systemService`.
// ---------------------------------------------------------------------------

use conduit_admin_graphql::system as gql_system;
use conduit_admin_graphql::system::SystemSettingsError as SSErr;

struct SystemSettingsAdapter {
    system: Arc<DomainSystemService>,
    /// Commercial price state lives outside `SystemService`; the adapter owns
    /// the shared PostgreSQL pool so it can derive the accounting-currency lock
    /// from the authoritative retail and procurement tables.
    pool: PgPool,
    /// Shared HTTP client for the GitHub release check (Go builds a fresh
    /// 10s-timeout client per call; reqwest clients are cheap to clone/share).
    http: reqwest::Client,
    /// Process start marker for the `uptime` field (Go `build.StartTime`).
    started_at: std::time::Instant,
}

/// Storage-wire shape of the retry policy — the full Go `biz.RetryPolicy`
/// JSON (snake_case tags, `system.go:309-341`). The domain
/// `conduit_services::RetryPolicy` only types the two timeout fields and keeps
/// the rest in its `extra` flatten-map, so the adapter round-trips through
/// this fully-typed wire struct via `serde_json::Value`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireRetryPolicy {
    enabled: bool,
    max_channel_retries: i32,
    max_single_channel_retries: i32,
    retry_delay_ms: i32,
    stream_first_event_timeout_seconds: u64,
    non_stream_response_timeout_seconds: u64,
    load_balancer_strategy: String,
    cost_score_weight: i32,
    auto_disable_channel: WireAutoDisableChannel,
    empty_response_detection: bool,
    upstream_error_policy: WireUpstreamErrorPolicy,
}

impl Default for WireRetryPolicy {
    /// Mirrors Go `defaultRetryPolicy` (`system_default.go:22-31`): retries
    /// enabled, 3 channel / 2 single-channel retries, 1s delay, adaptive LB,
    /// passthrough upstream errors.
    fn default() -> Self {
        Self {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 1000,
            stream_first_event_timeout_seconds: 0,
            non_stream_response_timeout_seconds: 0,
            load_balancer_strategy: "adaptive".to_string(),
            cost_score_weight: 0,
            auto_disable_channel: WireAutoDisableChannel::default(),
            empty_response_detection: false,
            upstream_error_policy: WireUpstreamErrorPolicy {
                // Go `UpstreamErrorModePassthrough` (`system.go:298`).
                mode: "passthrough".to_string(),
                custom_message: String::new(),
            },
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireAutoDisableChannel {
    enabled: bool,
    statuses: Vec<WireAutoDisableChannelStatus>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireAutoDisableChannelStatus {
    status: i32,
    times: i32,
}

fn validate_wire_auto_disable_channel(config: &WireAutoDisableChannel) -> Result<(), String> {
    let mut seen = std::collections::HashSet::with_capacity(config.statuses.len());
    for (index, rule) in config.statuses.iter().enumerate() {
        if !(400..=599).contains(&rule.status) {
            return Err(format!(
                "auto-disable statuses[{index}].status must be between 400 and 599"
            ));
        }
        if !(1..=100).contains(&rule.times) {
            return Err(format!(
                "auto-disable statuses[{index}].times must be between 1 and 100"
            ));
        }
        if !seen.insert(rule.status) {
            return Err(format!(
                "auto-disable status {} is configured more than once",
                rule.status
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireUpstreamErrorPolicy {
    mode: String,
    custom_message: String,
}

/// Storage-wire shape of `biz.SystemGeneralSettings` (`system.go:122-127`,
/// snake_case tags). The domain service has no typed accessor for this key,
/// so the adapter reads/writes it through `get_json`/`set_json` directly.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireGeneralSettings {
    accounting_currency_code: String,
    timezone: String,
    credit_display_name: String,
    credits_per_accounting_unit: rust_decimal::Decimal,
    exchange_rates: Vec<conduit_core::objects::money::CurrencyExchangeRate>,
    accounting_rate_version: u64,
}

impl Default for WireGeneralSettings {
    /// Mirrors Go `defaultGeneralSettings` (`system_default.go:52-55`).
    fn default() -> Self {
        Self {
            accounting_currency_code:
                conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE.to_string(),
            timezone: "UTC".to_string(),
            credit_display_name: conduit_core::objects::money::DEFAULT_CREDIT_DISPLAY_NAME
                .to_string(),
            credits_per_accounting_unit: rust_decimal::Decimal::from(10_000),
            exchange_rates: Vec::new(),
            accounting_rate_version: 1,
        }
    }
}

impl WireRetryPolicy {
    fn into_gql(self) -> gql_system::RetryPolicy {
        gql_system::RetryPolicy {
            max_channel_retries: self.max_channel_retries,
            max_single_channel_retries: self.max_single_channel_retries,
            retry_delay_ms: self.retry_delay_ms,
            stream_first_event_timeout_seconds: i32::try_from(
                self.stream_first_event_timeout_seconds,
            )
            .unwrap_or(i32::MAX),
            non_stream_response_timeout_seconds: i32::try_from(
                self.non_stream_response_timeout_seconds,
            )
            .unwrap_or(i32::MAX),
            load_balancer_strategy: self.load_balancer_strategy,
            cost_score_weight: self.cost_score_weight,
            enabled: self.enabled,
            auto_disable_channel: gql_system::AutoDisableChannel {
                enabled: self.auto_disable_channel.enabled,
                statuses: self
                    .auto_disable_channel
                    .statuses
                    .into_iter()
                    .map(|s| gql_system::AutoDisableChannelStatus {
                        status: s.status,
                        times: s.times,
                    })
                    .collect(),
            },
            empty_response_detection: self.empty_response_detection,
            upstream_error_policy: gql_system::UpstreamErrorPolicy {
                mode: self.upstream_error_policy.mode,
                custom_message: self.upstream_error_policy.custom_message,
            },
        }
    }

    fn from_gql(policy: gql_system::RetryPolicy) -> Self {
        Self {
            enabled: policy.enabled,
            max_channel_retries: policy.max_channel_retries,
            max_single_channel_retries: policy.max_single_channel_retries,
            retry_delay_ms: policy.retry_delay_ms,
            // Negative GraphQL input degrades to 0 (= disabled), matching the
            // Go service clamp lower bound.
            stream_first_event_timeout_seconds: u64::try_from(
                policy.stream_first_event_timeout_seconds.max(0),
            )
            .unwrap_or(0),
            non_stream_response_timeout_seconds: u64::try_from(
                policy.non_stream_response_timeout_seconds.max(0),
            )
            .unwrap_or(0),
            load_balancer_strategy: policy.load_balancer_strategy,
            cost_score_weight: policy.cost_score_weight.clamp(0, 100),
            auto_disable_channel: WireAutoDisableChannel {
                enabled: policy.auto_disable_channel.enabled,
                statuses: policy
                    .auto_disable_channel
                    .statuses
                    .into_iter()
                    .map(|s| WireAutoDisableChannelStatus {
                        status: s.status,
                        times: s.times,
                    })
                    .collect(),
            },
            empty_response_detection: policy.empty_response_detection,
            upstream_error_policy: WireUpstreamErrorPolicy {
                mode: policy.upstream_error_policy.mode,
                custom_message: policy.upstream_error_policy.custom_message,
            },
        }
    }
}

/// `true` if `tag` carries a prerelease marker. Mirrors Go `isPreReleaseTag`
/// (`version.go:169-181`).
fn is_prerelease_tag(tag: &str) -> bool {
    let lower = tag.to_lowercase();
    ["-beta", "-rc", "-alpha", "-dev", "-preview", "-snapshot"]
        .iter()
        .any(|p| lower.contains(p))
}

/// Parse the numeric core of a `vX.Y.Z` / `X.Y.Z` tag. Missing minor/patch
/// default to 0 (Go's semver lib accepts partial versions the same way).
fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// `true` when `latest` is strictly newer than `current`. Parse failures →
/// `false`, mirroring Go `IsNewerVersion` (`version.go:184-199`).
fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Human-readable uptime, approximating Go `time.Duration.String()` for the
/// whole-second granularity the frontend displays (`"3h2m5s"`).
fn format_uptime(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

impl SystemSettingsAdapter {
    async fn accounting_currency_locked(&self) -> Result<bool, SSErr> {
        postgres_accounting_currency_locked(&self.pool)
            .await
            .map_err(SSErr::GeneralSettings)
    }

    /// Fetch the latest stable release tag from GitHub. Mirrors Go
    /// `FetchLatestGitHubRelease` (`version.go:96-158`): first non-draft,
    /// non-prerelease `v*` tag without prerelease markers, past a 30-minute
    /// publish cooldown.
    async fn fetch_latest_github_release(&self) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct GitHubRelease {
            tag_name: String,
            #[serde(default)]
            prerelease: bool,
            #[serde(default)]
            draft: bool,
            #[serde(default)]
            published_at: Option<chrono::DateTime<chrono::Utc>>,
        }

        let resp = self
            .http
            .get("https://api.github.com/repos/404F0X/Conduit-API/releases?per_page=10&page=1")
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "Conduit API-Version-Checker")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|err| format!("failed to fetch releases: {err}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned status {}",
                resp.status().as_u16()
            ));
        }
        let releases: Vec<GitHubRelease> = resp
            .json()
            .await
            .map_err(|err| format!("failed to decode releases: {err}"))?;

        let now = chrono::Utc::now();
        for release in releases {
            if release.draft || release.prerelease {
                continue;
            }
            // Conduit API tags are bare `vX.Y.Z`; service-prefixed monorepo
            // tags (for example, `component/v1.0.0`) are skipped.
            if !release.tag_name.starts_with('v') {
                continue;
            }
            if is_prerelease_tag(&release.tag_name) {
                continue;
            }
            // 30-minute cooldown for build/upload to finish (Go
            // `releaseCooldownDuration`).
            if let Some(at) = release.published_at
                && now - at < chrono::Duration::minutes(30)
            {
                continue;
            }
            return Ok(release.tag_name);
        }
        Err("no stable release found".to_string())
    }
}

/// Serialize accounting-currency identity changes with every retail or channel
/// price write for the lifetime of the caller's transaction.
pub(crate) async fn lock_accounting_currency_price_writes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(786222001)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Whether changing the accounting-currency identity would relabel any stored
/// numeric price. History tables intentionally count too: archived versions
/// still carry immutable currency semantics even when no current head is live.
async fn postgres_accounting_currency_locked(pool: &PgPool) -> Result<bool, String> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM price_books) \
             OR EXISTS(SELECT 1 FROM price_book_versions) \
             OR EXISTS(SELECT 1 FROM channel_model_prices) \
             OR EXISTS(SELECT 1 FROM channel_model_price_versions)",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| format!("failed to inspect stored pricing state: {error}"))
}

async fn postgres_accounting_currency_locked_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM price_books) \
             OR EXISTS(SELECT 1 FROM price_book_versions) \
             OR EXISTS(SELECT 1 FROM channel_model_prices) \
             OR EXISTS(SELECT 1 FROM channel_model_price_versions)",
    )
    .fetch_one(&mut **tx)
    .await
}

#[async_trait]
impl gql_system::SystemSettingsServices for SystemSettingsAdapter {
    async fn system_version(&self) -> Result<gql_system::SystemVersion, SSErr> {
        Ok(gql_system::SystemVersion {
            version: option_env!("CONDUIT_BUILD_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_string(),
            commit: option_env!("CONDUIT_BUILD_COMMIT")
                .unwrap_or("unknown")
                .to_string(),
            build_time: option_env!("CONDUIT_BUILD_TIME").unwrap_or("").to_string(),
            rust_version: option_env!("CONDUIT_BUILD_RUSTC_VERSION")
                .unwrap_or("unknown")
                .to_string(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            uptime: format_uptime(self.started_at.elapsed()),
        })
    }

    async fn check_for_update(&self) -> Result<gql_system::VersionCheck, SSErr> {
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let latest_version = self
            .fetch_latest_github_release()
            .await
            .unwrap_or_else(|_| current_version.clone());
        let has_update = is_newer_version(&current_version, &latest_version);
        let release_url =
            format!("https://github.com/404F0X/Conduit-API/releases/tag/{latest_version}");
        Ok(gql_system::VersionCheck {
            current_version,
            latest_version,
            has_update,
            release_url,
        })
    }

    async fn proxy_presets(&self) -> Result<Vec<gql_system::ProxyPreset>, SSErr> {
        let ctx = boot_request_context();
        // Masked view: passwords come back as `"****"` (Go masks inside the
        // service before the API boundary).
        let presets = self
            .system
            .masked_proxy_presets(&ctx)
            .await
            .map_err(|err| SSErr::ProxyPresets(err.to_string()))?;
        Ok(presets
            .into_iter()
            .map(|p| gql_system::ProxyPreset {
                name: p.name,
                url: p.url,
                username: p.username,
                password: p.password,
            })
            .collect())
    }

    async fn security_settings(&self) -> Result<gql_system::SecuritySettings, SSErr> {
        let ctx = boot_request_context();
        let settings = self
            .system
            .security_settings(&ctx)
            .await
            .map_err(|err| SSErr::ReadSecurity(err.to_string()))?;
        Ok(gql_system::SecuritySettings {
            blocked_ips: settings.blocked_ips,
            show_request_log_ip_ban_icon: settings.show_request_log_ip_ban_icon,
        })
    }

    async fn onboarding_record(&self) -> Result<Option<gql_system::OnboardingRecord>, SSErr> {
        let ctx = boot_request_context();
        let record = self
            .system
            .onboarding_info(&ctx)
            .await
            .map_err(|err| SSErr::OnboardingInfo(err.to_string()))?;
        Ok(record.map(|r| gql_system::OnboardingRecord {
            onboarded: r.onboarded,
            completed_at: r.completed_at,
            system_model_setting: r
                .system_model_setting
                .map(|m| gql_system::OnboardingModule {
                    onboarded: m.onboarded,
                    completed_at: m.completed_at,
                }),
            auto_disable_channel: r
                .auto_disable_channel
                .map(|m| gql_system::OnboardingModule {
                    onboarded: m.onboarded,
                    completed_at: m.completed_at,
                }),
        }))
    }

    async fn complete_onboarding(&self) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .complete_onboarding(&ctx)
            .await
            .map_err(|err| SSErr::CompleteOnboarding(err.to_string()))
    }

    async fn set_security_settings(
        &self,
        settings: gql_system::SecuritySettings,
    ) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        let domain = conduit_services::SecuritySettings {
            blocked_ips: settings.blocked_ips,
            show_request_log_ip_ban_icon: settings.show_request_log_ip_ban_icon,
            extra: std::collections::BTreeMap::new(),
        };
        self.system
            .set_security_settings(&ctx, domain)
            .await
            .map(|_| ())
            .map_err(|err| SSErr::UpdateSecurity(err.to_string()))
    }

    async fn save_proxy_preset(&self, preset: gql_system::ProxyPreset) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        let domain = conduit_services::ProxyPreset {
            name: preset.name,
            url: preset.url,
            username: preset.username,
            password: preset.password,
            extra: std::collections::BTreeMap::new(),
        };
        self.system
            .save_proxy_preset(&ctx, domain)
            .await
            .map_err(|err| SSErr::SaveProxyPreset(err.to_string()))
    }

    async fn delete_proxy_preset(&self, url: &str) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .delete_proxy_preset(&ctx, url)
            .await
            .map_err(|err| SSErr::DeleteProxyPreset(err.to_string()))
    }

    async fn brand_name(&self) -> Result<String, SSErr> {
        let ctx = boot_request_context();
        self.system
            .brand_name(&ctx)
            .await
            .map_err(|err| SSErr::BrandName(err.to_string()))
    }

    async fn brand_logo(&self) -> Result<String, SSErr> {
        let ctx = boot_request_context();
        self.system
            .brand_logo(&ctx)
            .await
            .map_err(|err| SSErr::BrandLogo(err.to_string()))
    }

    async fn title(&self) -> Result<String, SSErr> {
        // Go `SystemService.Title` (`system.go:856-870`): not-found → "".
        let ctx = boot_request_context();
        Ok(self
            .system
            .get_system_value(&ctx, system_key::TITLE)
            .await
            .map_err(|err| SSErr::Title(err.to_string()))?
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    async fn set_brand_name(&self, name: &str) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .set_brand_name(&ctx, name)
            .await
            .map_err(|err| SSErr::UpdateBrandName(err.to_string()))
    }

    async fn set_brand_logo(&self, logo: &str) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .set_brand_logo(&ctx, logo)
            .await
            .map_err(|err| SSErr::UpdateBrandLogo(err.to_string()))
    }

    async fn set_title(&self, title: &str) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .set_system_value(
                &ctx,
                system_key::TITLE,
                serde_json::Value::from(title.to_string()),
            )
            .await
            .map(|_| ())
            .map_err(|err| SSErr::UpdateTitle(err.to_string()))
    }

    async fn storage_policy(&self) -> Result<gql_system::StoragePolicy, SSErr> {
        let ctx = boot_request_context();
        let policy = self
            .system
            .storage_policy(&ctx)
            .await
            .map_err(|err| SSErr::StoragePolicy(err.to_string()))?;
        Ok(gql_system::StoragePolicy {
            store_chunks: policy.store_chunks,
            live_preview: policy.live_preview,
            store_request_headers: policy.store_request_headers,
            store_request_body: policy.store_request_body,
            store_response_body: policy.store_response_body,
            cleanup_options: policy
                .cleanup_options
                .into_iter()
                .map(|o| gql_system::CleanupOption {
                    resource_type: o.resource_type,
                    enabled: o.enabled,
                    cleanup_days: i32::try_from(o.cleanup_days).unwrap_or(i32::MAX),
                })
                .collect(),
        })
    }

    async fn set_storage_policy(&self, policy: gql_system::StoragePolicy) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        let domain = conduit_services::StoragePolicy {
            store_chunks: policy.store_chunks,
            live_preview: policy.live_preview,
            store_request_headers: policy.store_request_headers,
            store_request_body: policy.store_request_body,
            store_response_body: policy.store_response_body,
            cleanup_options: policy
                .cleanup_options
                .into_iter()
                .map(|o| conduit_services::CleanupOption {
                    resource_type: o.resource_type,
                    enabled: o.enabled,
                    cleanup_days: i64::from(o.cleanup_days),
                })
                .collect(),
        };
        self.system
            .set_storage_policy(&ctx, &domain)
            .await
            .map(|_| ())
            .map_err(|err| SSErr::UpdateStoragePolicy(err.to_string()))
    }

    async fn retry_policy(&self) -> Result<gql_system::RetryPolicy, SSErr> {
        let ctx = boot_request_context();
        let stored = self
            .system
            .retry_policy(&ctx)
            .await
            .map_err(|err| SSErr::RetryPolicy(err.to_string()))?;
        // Stored policy → full wire shape (typed timeouts + `extra` flatten
        // re-serialize to the Go snake_case JSON); missing key → Go
        // `defaultRetryPolicy`.
        let wire = match stored {
            Some(policy) => serde_json::to_value(&policy)
                .and_then(serde_json::from_value::<WireRetryPolicy>)
                .map_err(|err| SSErr::RetryPolicy(err.to_string()))?,
            None => WireRetryPolicy::default(),
        };
        Ok(wire.into_gql())
    }

    async fn set_retry_policy(&self, policy: gql_system::RetryPolicy) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        // Full wire JSON → domain struct (timeouts land in the typed fields
        // and get clamped by the service; the rest rides in `extra`).
        let wire = WireRetryPolicy::from_gql(policy);
        validate_wire_auto_disable_channel(&wire.auto_disable_channel)
            .map_err(SSErr::UpdateRetryPolicy)?;
        let domain: conduit_services::RetryPolicy = serde_json::to_value(wire)
            .and_then(serde_json::from_value)
            .map_err(|err| SSErr::UpdateRetryPolicy(err.to_string()))?;
        self.system
            .set_retry_policy(&ctx, domain)
            .await
            .map(|_| ())
            .map_err(|err| SSErr::UpdateRetryPolicy(err.to_string()))
    }

    async fn user_agent_pass_through(&self) -> Result<bool, SSErr> {
        let ctx = boot_request_context();
        self.system
            .user_agent_pass_through(&ctx)
            .await
            .map_err(|err| SSErr::UserAgentPassThrough(err.to_string()))
    }

    async fn set_user_agent_pass_through(&self, enabled: bool) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .set_user_agent_pass_through(&ctx, enabled)
            .await
            .map_err(|err| SSErr::UpdateUserAgentPassThrough(err.to_string()))
    }

    async fn default_data_storage_id(&self) -> Result<i64, SSErr> {
        let ctx = boot_request_context();
        self.system
            .default_data_storage_id(&ctx)
            .await
            .map_err(|err| SSErr::DefaultDataStorageID(err.to_string()))
    }

    async fn set_default_data_storage_id(&self, id: i64) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        self.system
            .set_default_data_storage_id(&ctx, id)
            .await
            .map_err(|err| SSErr::UpdateDefaultDataStorage(err.to_string()))
    }

    async fn general_settings(&self) -> Result<gql_system::SystemGeneralSettings, SSErr> {
        // Go `GeneralSettings` (`system.go:1291-…`): missing key →
        // `defaultGeneralSettings` (USD/UTC). No typed domain accessor yet, so
        // read the JSON key directly.
        let ctx = boot_request_context();
        let wire = self
            .system
            .get_json::<WireGeneralSettings>(&ctx, system_key::GENERAL_SETTINGS)
            .await
            .map_err(|err| SSErr::GeneralSettings(err.to_string()))?
            .unwrap_or_default();
        let accounting_currency_locked = self.accounting_currency_locked().await?;
        Ok(gql_system::SystemGeneralSettings {
            accounting_currency_code: wire.accounting_currency_code,
            accounting_currency_locked,
            timezone: wire.timezone,
            credit_display_name: wire.credit_display_name,
            credits_per_accounting_unit: conduit_admin_graphql::scalars::DecimalScalar(
                wire.credits_per_accounting_unit,
            ),
            exchange_rates: wire
                .exchange_rates
                .into_iter()
                .map(|rate| gql_system::CurrencyExchangeRate {
                    currency_code: rate.currency,
                    quote_per_accounting_unit: conduit_admin_graphql::scalars::DecimalScalar(
                        rate.quote_per_accounting_unit,
                    ),
                })
                .collect(),
            accounting_rate_version: i64::try_from(wire.accounting_rate_version)
                .unwrap_or(i64::MAX),
        })
    }

    async fn set_general_settings(
        &self,
        actor_user_id: Option<i64>,
        settings: gql_system::SystemGeneralSettings,
    ) -> Result<(), SSErr> {
        let requested_currency = settings
            .accounting_currency_code
            .trim()
            .to_ascii_uppercase();
        let accounting_settings = conduit_core::objects::money::AccountingSettings {
            accounting_currency: requested_currency.clone(),
            credit_display_name: settings.credit_display_name.clone(),
            credits_per_accounting_unit: settings.credits_per_accounting_unit.0,
            exchange_rates: settings
                .exchange_rates
                .iter()
                .map(|rate| conduit_core::objects::money::CurrencyExchangeRate {
                    currency: rate.currency_code.clone(),
                    quote_per_accounting_unit: rate.quote_per_accounting_unit.0,
                })
                .collect(),
            version: u64::try_from(settings.accounting_rate_version).unwrap_or(1),
        };
        accounting_settings
            .validate()
            .map_err(SSErr::UpdateGeneralSettings)?;

        let wire = WireGeneralSettings {
            accounting_currency_code: requested_currency.clone(),
            timezone: settings.timezone,
            credit_display_name: settings.credit_display_name,
            credits_per_accounting_unit: settings.credits_per_accounting_unit.0,
            exchange_rates: settings
                .exchange_rates
                .into_iter()
                .map(|rate| conduit_core::objects::money::CurrencyExchangeRate {
                    currency: rate.currency_code,
                    quote_per_accounting_unit: rate.quote_per_accounting_unit.0,
                })
                .collect(),
            accounting_rate_version: u64::try_from(chrono::Utc::now().timestamp_millis())
                .unwrap_or(1),
        };
        let encoded = serde_json::to_string(&wire)
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;

        // Currency identity and every price write share this transaction lock.
        // The current-value read, price-existence recheck, and settings upsert
        // therefore form one indivisible decision.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        let current = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 LIMIT 1",
        )
        .bind(system_key::GENERAL_SETTINGS)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?
        .map(|value| serde_json::from_str::<WireGeneralSettings>(&value))
        .transpose()
        .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?
        .unwrap_or_default();
        if !current
            .accounting_currency_code
            .trim()
            .eq_ignore_ascii_case(&requested_currency)
            && postgres_accounting_currency_locked_in_tx(&mut tx)
                .await
                .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?
        {
            return Err(SSErr::UpdateGeneralSettings(
                "accounting currency cannot be changed after any retail or channel procurement price exists; rebuild pricing data before changing it"
                    .to_string(),
            ));
        }
        let before_snapshot = serde_json::to_value(&current)
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        let after_snapshot = serde_json::to_value(&wire)
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO systems (key,value,created_at,updated_at) VALUES ($1,$2,$3,$3) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, \
             updated_at=excluded.updated_at,deleted_at=0",
        )
        .bind(system_key::GENERAL_SETTINGS)
        .bind(encoded)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        insert_pricing_change_audit(
            &mut tx,
            actor_user_id,
            "update_accounting_settings",
            "accounting_settings",
            system_key::GENERAL_SETTINGS,
            Some(before_snapshot),
            Some(after_snapshot),
            &requested_currency,
            wire.accounting_rate_version,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        tx.commit()
            .await
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))?;
        self.system
            .invalidate_system_value_cache(system_key::GENERAL_SETTINGS)
            .await
            .map_err(|err| SSErr::UpdateGeneralSettings(err.to_string()))
    }

    async fn model_settings(&self) -> Result<gql_system::SystemModelSettings, SSErr> {
        let ctx = boot_request_context();
        let settings = self
            .system
            .model_settings(&ctx)
            .await
            .map_err(|err| SSErr::ModelSettings(err.to_string()))?;
        Ok(gql_system::SystemModelSettings {
            fallback_to_channels_on_model_not_found: settings
                .fallback_to_channels_on_model_not_found,
            query_all_channel_models: settings.query_all_channel_models,
            default_model_api_include_all: settings.default_model_api_include_all,
            auto_reasoning_effort: settings.auto_reasoning_effort,
            model_blacklist_regex: settings.model_blacklist_regex,
            developer_settings: settings
                .developer_settings
                .into_iter()
                .map(|d| gql_system::DeveloperModelSettings {
                    developer: d.developer,
                    associations: d
                        .associations
                        .into_iter()
                        .map(crate::conv::model_association_to_gql)
                        .collect(),
                })
                .collect(),
        })
    }

    async fn set_model_settings(
        &self,
        settings: gql_system::SystemModelSettings,
    ) -> Result<(), SSErr> {
        let ctx = boot_request_context();
        let domain = conduit_core::objects::SystemModelSettings {
            fallback_to_channels_on_model_not_found: settings
                .fallback_to_channels_on_model_not_found,
            query_all_channel_models: settings.query_all_channel_models,
            default_model_api_include_all: settings.default_model_api_include_all,
            auto_reasoning_effort: settings.auto_reasoning_effort,
            model_blacklist_regex: settings.model_blacklist_regex,
            developer_settings: settings
                .developer_settings
                .into_iter()
                .map(|d| conduit_core::objects::DeveloperModelSettings {
                    developer: d.developer,
                    associations: d
                        .associations
                        .into_iter()
                        .map(crate::conv::model_association_to_core)
                        .collect(),
                })
                .collect(),
        };
        self.system
            .set_model_settings(&ctx, domain)
            .await
            .map_err(|err| SSErr::UpdateModelSettings(err.to_string()))
    }
}

// ---------------------------------------------------------------------------
// ChannelExtraQuery adapter — backs the admin GraphQL channels list-page root
// queries (CONV-CH): `allChannelSummarys` / `allChannelTags` /
// `countChannelsByType` / `queryChannels`. Delegates to the same live
// the channel repository the orchestrator's candidate source uses, and reuses
// Feng's `crate::conv::channel_row_to_gql` (CONV-01) to bridge `ChannelRow`
// into the GraphQL `Channel`.
//
// ## Project-profile visibility (deliberate divergence, documented)
//
// The Go resolvers (`conduit.resolvers.go:674-731`) additionally filter the
// result by the caller's project active-profile WHEN a project id is present in
// context (`contexts.GetProjectID`). The admin schema is a boot-time singleton
// with no per-request project id in its data bag, so — exactly like Go's
// `!ok || projectID == 0` early-return branch — this adapter performs NO
// project-profile filtering. Admin/owner callers (who drive the channels list
// page) hit that same no-project branch in Go, so the observable result matches.
// A future per-request project-scoped path would inject the profile filter here.
// ---------------------------------------------------------------------------

/// GraphQL-facing [`ChannelExtraQueryServices`] adapter backed by the live
/// [`ChannelRepo`].
struct ChannelExtraQueryAdapter {
    channel_repo: Arc<dyn ChannelRepo>,
    price_repo: Arc<dyn ChannelModelPriceRepo>,
}

/// Map the GraphQL `ChannelStatus` enum to the domain wire literal
/// (`enabled`/`disabled`/`archived`) the `channels.status` column stores.
fn channel_status_to_wire(status: GqlChannelStatus) -> &'static str {
    match status {
        GqlChannelStatus::Enabled => "enabled",
        GqlChannelStatus::Disabled => "disabled",
        GqlChannelStatus::Archived => "archived",
    }
}

impl ChannelExtraQueryAdapter {
    /// Load every channel matching `status_in` (empty = no status filter),
    /// paging through the repo in generous windows. The channels table is
    /// small (a gateway realistically has tens of channels), so a full
    /// in-memory materialization mirrors Go's `.All(ctx)` faithfully without a
    /// streaming cursor. Returns the raw rows for the caller to shape.
    async fn load_all(
        &self,
        status_in: Vec<String>,
    ) -> Result<Vec<conduit_db::row::ChannelRow>, ChannelExtraQueryError> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        // A page size large enough that one round-trip covers realistic
        // catalogs; the loop keeps correctness if a deployment ever exceeds it.
        const PAGE: u32 = 500;
        loop {
            let query = ListChannelsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
                status_in: status_in.clone(),
            };
            let result = self
                .channel_repo
                .list_channels(&ctx, &query)
                .await
                .map_err(|e| ChannelExtraQueryError::QueryChannels(e.to_string()))?;
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
impl ChannelExtraQueryServices for ChannelExtraQueryAdapter {
    async fn channel_model_prices(
        &self,
        channel_id: &str,
    ) -> Result<Vec<conduit_admin_graphql::model_ext::ChannelModelPrice>, ChannelExtraQueryError>
    {
        let id = channel_id
            .rsplit('/')
            .next()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| ChannelExtraQueryError::Query("invalid channel id".into()))?;
        let rows = self
            .price_repo
            .list_prices_by_channel(&boot_request_context(), id)
            .await
            .map_err(|error| ChannelExtraQueryError::Query(error.to_string()))?;
        rows.into_iter()
            .map(|row| {
                price_row_to_gql(row)
                    .map_err(|error| ChannelExtraQueryError::Query(error.to_string()))
            })
            .collect()
    }

    async fn all_channel_summarys(
        &self,
        include_archived: bool,
    ) -> Result<Vec<conduit_admin_graphql::channel::Channel>, ChannelExtraQueryError> {
        // Go: status filter `[enabled, disabled]` (+ archived when requested),
        // ordered Desc by ordering_weight (conduit.resolvers.go:674-701).
        let mut status_in = vec!["enabled".to_string(), "disabled".to_string()];
        if include_archived {
            status_in.push("archived".to_string());
        }
        let mut rows = self.load_all(status_in).await?;
        // ent.Desc(FieldOrderingWeight). The repo returns created_at-asc order,
        // so re-sort by ordering_weight descending (stable on ties).
        rows.sort_by(|a, b| b.ordering_weight.cmp(&a.ordering_weight));
        Ok(rows
            .into_iter()
            .map(crate::conv::channel_row_to_gql)
            .collect())
    }

    async fn all_channel_tags(&self) -> Result<Vec<String>, ChannelExtraQueryError> {
        // Go: StatusNEQ(archived) → flatten tags → lo.Uniq (first-seen order).
        let rows = self
            .load_all(vec!["enabled".to_string(), "disabled".to_string()])
            .await?;
        let mut seen: Vec<String> = Vec::new();
        for row in &rows {
            for tag in &row.tags {
                if !seen.contains(tag) {
                    seen.push(tag.clone());
                }
            }
        }
        Ok(seen)
    }

    async fn count_channels_by_type(
        &self,
        args: CountChannelsByTypeArgs,
    ) -> Result<Vec<ChannelTypeCount>, ChannelExtraQueryError> {
        // Go: non-empty statusIn → StatusIn(...); empty/absent →
        // StatusNEQ(archived). GroupBy(type).Count() (conduit.resolvers.go:733-763).
        let status_in: Vec<String> = match &args.status_in {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|s| channel_status_to_wire(*s).to_string())
                .collect(),
            // Empty/absent → all non-archived (enabled + disabled).
            _ => vec!["enabled".to_string(), "disabled".to_string()],
        };
        let rows = self.load_all(status_in).await?;
        // GroupBy(type).Aggregate(Count()). BTreeMap keeps a deterministic
        // (type-sorted) order; Go's ent scan order is unspecified so the
        // frontend does not depend on it.
        let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for row in &rows {
            *counts.entry(row.channel_type.clone()).or_insert(0) += 1;
        }
        Ok(counts
            .into_iter()
            .map(|(channel_type, count)| ChannelTypeCount {
                channel_type,
                count,
            })
            .collect())
    }

    async fn query_channels(
        &self,
        args: QueryChannelsArgs,
    ) -> Result<GqlChannelConnection, ChannelExtraQueryError> {
        use conduit_admin_graphql::pagination::{
            connection_from_offset_page, decode_offset_cursor,
        };
        use conduit_admin_graphql::scalars::CursorScalar;

        // The `where.statusIn` predicate is the one filter the channels list page
        // actually drives; lower it to the repo status filter. Other `where`
        // predicates + the `hasTag` / `model` filters are applied in-memory below
        // (Go's `model` filter also bypasses DB pagination — biz/channel_query.go).
        let status_in: Vec<String> =
            match args.where_filter.as_ref().and_then(|w| w.status_in.clone()) {
                Some(list) if !list.is_empty() => list
                    .iter()
                    .map(|s| channel_status_to_wire(*s).to_string())
                    .collect(),
                _ => Vec::new(),
            };
        let mut rows = self.load_all(status_in).await?;

        // Apply the complete ChannelWhereInput predicate family after the repo
        // has narrowed the common status-in case. `queryChannels` and
        // `channels` must share this matcher; otherwise JSON-backed fields such
        // as settings.managementAdapter silently work on one root field while
        // being ignored by the channels list page.
        if let Some(where_filter) = &args.where_filter {
            rows.retain(|row| {
                crate::wiring_channel_crud::channel_row_matches_where(row, where_filter)
            });
        }

        // `hasTag` — JSON-contains on the tags column (Go: tag membership).
        if let Some(tag) = &args.has_tag {
            rows.retain(|r| r.tags.iter().any(|t| t == tag));
        }

        // `model` — in-memory `IsModelSupported` filter (Go bypasses pagination
        // for this; we mirror by filtering the full set on supported_models).
        if let Some(model) = &args.model {
            rows.retain(|r| r.supported_models.iter().any(|m| m == model));
        }

        let total_count = rows.len() as i64;

        // Convert to GraphQL Channels, then apply Relay offset pagination.
        let channels: Vec<conduit_admin_graphql::channel::Channel> = rows
            .into_iter()
            .map(crate::conv::channel_row_to_gql)
            .collect();

        // `after` cursor → start offset (offset-based, matching the crate's
        // `connection_from_offset_page` cursor scheme). A malformed cursor
        // degrades to offset 0 rather than failing the whole query.
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset)
            .unwrap_or(0)
            .min(channels.len());
        let windowed = channels[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

        Ok(GqlChannelConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(GqlChannelEdge {
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

    async fn channel_sensitive_fields(
        &self,
        channel_id: &str,
    ) -> Result<Option<ChannelSensitiveFields>, ChannelExtraQueryError> {
        let db_id = if let Ok(guid) = conduit_admin_graphql::node::parse_guid(channel_id) {
            guid.id.to_string()
        } else if channel_id.parse::<i64>().is_ok() {
            channel_id.to_owned()
        } else {
            return Ok(None);
        };
        let row = self
            .channel_repo
            .find_channel(&boot_request_context(), &db_id)
            .await
            .map_err(|err| ChannelExtraQueryError::Query(err.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };

        let credentials: conduit_core::objects::channel_settings::ChannelCredentials =
            serde_json::from_value(row.credentials).unwrap_or_default();
        let is_oauth = credentials.is_oauth() || row.channel_type == "antigravity";
        let gql_credentials = conduit_admin_graphql::channel::ChannelCredentials {
            api_key: if is_oauth && !credentials.api_key.is_empty() {
                Some(credentials.api_key.clone())
            } else {
                None
            },
            api_keys: if is_oauth {
                None
            } else {
                credentials.get_all_api_keys()
            },
            gcp: credentials
                .gcp
                .map(|gcp| conduit_admin_graphql::channel::GCPCredential {
                    region: gcp.region,
                    project_id: gcp.project_id,
                    json_data: gcp.json_data,
                }),
            oauth: None,
        };

        let disabled: Vec<conduit_core::objects::channel_settings::DisabledAPIKey> =
            serde_json::from_value(row.disabled_api_keys).unwrap_or_default();
        let disabled_api_keys = disabled
            .into_iter()
            .map(|key| conduit_admin_graphql::channel::DisabledAPIKey {
                key: key.key,
                disabled_at: conduit_admin_graphql::scalars::TimeScalar(key.disabled_at),
                error_code: key.error_code,
                reason: (!key.reason.is_empty()).then_some(key.reason),
            })
            .collect();

        Ok(Some(ChannelSensitiveFields {
            credentials: Some(gql_credentials),
            disabled_api_keys,
        }))
    }
}

/// GraphQL-facing [`ModelExtServices`] adapter — backs the GAP-B model
/// extended-domain resolvers.
///
/// ## Scope note (what is live vs deferred)
///
/// The Go resolvers split across four services (`modelService`,
/// `channelService`, `systemService`, plus an external model *fetcher*). On the
/// Rust side has live model, channel, and channel-price repositories. The
/// external model-fetcher HTTP client has not been ported, so this adapter
/// wires the repository-backed paths, including channel-price persistence:
///
///   - `query_models` — configured-models path (Go
///     `modelService.ListModels`): list models, filter by status, map to
///     `ModelIdentityWithStatus`. The "all channel models" branch (Go
///     `channelService.ListModels`, gated on `QueryAllChannelModels ||
///     input.includeAllChannelModels`) is NOT taken here — it needs the channel
///     model-entry expansion that lives in `channel_service` and is not yet
///     repo-backed; a request that sets `includeAllChannelModels` surfaces a
///     clear "not yet supported" error rather than silently returning the
///     configured set.
///   - `update_model_status` / `bulk_archive_models` / `bulk_disable_models` /
///     `bulk_enable_models` — set the model `status` column (Go
///     `SetStatus(...)`), iterating `ModelRepo::update_model` per id (the repo
///     has no batch-update surface; the Go `Update().Where(IDIn)` is one
///     statement, but per-row updates are semantically identical for status).
///   - `bulk_delete_models` — Go `Model.Delete().Where(IDIn)` is a HARD delete;
///     the Rust `ModelRepo` only exposes `soft_delete_model`. We soft-delete
///     each id (sets `deleted_at`), which hides the row from every default
///     query — the observable effect the admin UI depends on. This is a
///     deliberate, documented divergence from Go's hard delete (flagged for a
///     follow-up if hard-delete parity is required).
///
/// `fetch_models` remains deferred on that external client. The other extended
/// methods use the live repositories and row/object converters; price writes
/// resolve the accounting currency from system settings for each operation.
struct ModelExtAdapter {
    model_repo: Arc<dyn ModelRepo>,
    channel_repo: Arc<dyn ChannelRepo>,
    system: Arc<DomainSystemService>,
    pool: PgPool,
}

/// Map a GraphQL `ID` (either a `gid://conduit/Model/<n>` wire form or a bare
/// numeric string) to the numeric DB id string the repo expects. Mirrors Go
/// `objects.IntGuids` / `GUID.UnmarshalGQL` which accept the typed GUID form.
fn model_id_from_gql(id: &async_graphql::ID) -> Result<String, ModelExtError> {
    let raw = id.as_str();
    if let Ok(guid) = parse_guid(raw) {
        return Ok(guid.id.to_string());
    }
    // Fall back to a bare numeric id (some callers pass the raw db id).
    if raw.parse::<i64>().is_ok() {
        return Ok(raw.to_string());
    }
    Err(ModelExtError::UpdateModelStatus(format!(
        "invalid model id: {raw}"
    )))
}

/// Map the domain model `status` column value to the GraphQL `ChannelStatus`
/// enum. Go builds `ModelIdentityWithStatus.Status` as
/// `channel.Status(m.Status.String())` — the model status strings
/// (`enabled`/`disabled`/`archived`) are a superset-compatible subset of the
/// channel status strings, so the mapping is 1:1.
fn model_status_str_to_channel_status(status: &str) -> GqlChannelStatus {
    match status {
        "enabled" => GqlChannelStatus::Enabled,
        "archived" => GqlChannelStatus::Archived,
        // `disabled` and any unexpected value → Disabled (Go's model default).
        _ => GqlChannelStatus::Disabled,
    }
}

/// GraphQL `ModelStatus` enum → domain wire literal (`enabled`/`disabled`/
/// `archived`). Matches the `#[graphql(name = ...)]` spellings on the enum.
fn model_status_to_wire(status: GqlModelStatus) -> &'static str {
    match status {
        GqlModelStatus::Enabled => "enabled",
        GqlModelStatus::Disabled => "disabled",
        GqlModelStatus::Archived => "archived",
    }
}

// ---------------------------------------------------------------------------
// Channel-model-price helpers (saveChannelModelPrices + duplicateChannel).
// ---------------------------------------------------------------------------

/// Decode a GraphQL `Channel` id (either a `gid://conduit/Channel/<n>` GUID or
/// a bare numeric string) into the numeric DB key the price repo stores.
/// Mirrors Go `objects.IntGuid` decoding of the `channelID` argument.
fn channel_price_db_id(raw: &str) -> Option<i64> {
    if let Ok(guid) = parse_guid(raw) {
        return Some(guid.id);
    }
    raw.parse::<i64>().ok()
}

/// Port of Go `objects.Pricing.Validate` (`objects/price.go:80-107`): the
/// mode-specific required field must be present, and tiered/volume tiers must
/// obey the "only the last tier omits `upTo`" rule.
fn validate_pricing(p: &core_pricing::Pricing) -> Result<(), String> {
    match p.mode.as_str() {
        core_pricing::PRICING_MODE_FLAT_FEE => {
            let Some(value) = p.flat_fee else {
                return Err("flatFee is required".to_string());
            };
            if value.is_sign_negative() {
                return Err("flatFee must not be negative".to_string());
            }
        }
        core_pricing::PRICING_MODE_USAGE_PER_UNIT => {
            let Some(value) = p.usage_per_unit else {
                return Err("usagePerUnit is required".to_string());
            };
            if value.is_sign_negative() {
                return Err("usagePerUnit must not be negative".to_string());
            }
        }
        core_pricing::PRICING_MODE_TIERED | core_pricing::PRICING_MODE_VOLUME => {
            match &p.usage_tiered {
                None => return Err("usageTiered is required".to_string()),
                Some(t) => validate_tiered_pricing(t)?,
            }
        }
        other => return Err(format!("unknown pricing mode: {other}")),
    }
    Ok(())
}

/// Port of Go `TieredPricing.Validate` (`objects/price.go:109-135`).
fn validate_tiered_pricing(t: &core_pricing::TieredPricing) -> Result<(), String> {
    if t.tiers.is_empty() {
        return Err("tiers is required".to_string());
    }
    let last = t.tiers.len() - 1;
    let mut previous_up_to = 0_i64;
    for (i, tier) in t.tiers.iter().enumerate() {
        if tier.price_per_unit.is_sign_negative() {
            return Err(format!("tiers[{i}].pricePerUnit must not be negative"));
        }
        if i == last {
            if tier.up_to.is_some() {
                return Err(format!("tiers[{i}].upTo must be null"));
            }
        } else {
            let Some(up_to) = tier.up_to else {
                return Err(format!("tiers[{i}].upTo is required"));
            };
            if up_to <= previous_up_to {
                return Err(format!(
                    "tiers[{i}].upTo must be greater than {previous_up_to}"
                ));
            }
            previous_up_to = up_to;
        }
    }
    Ok(())
}

/// Port of Go `ModelPrice.Validate` (`objects/price.go:167-179`): validate each
/// item's pricing + every prompt-write-cache variant's pricing.
pub(crate) fn validate_model_price(p: &core_pricing::ModelPrice) -> Result<(), String> {
    for (idx, item) in p.items.iter().enumerate() {
        validate_pricing(&item.pricing).map_err(|e| format!("items[{idx}]: pricing: {e}"))?;
        for (vidx, variant) in item.prompt_write_cache_variants.iter().enumerate() {
            validate_pricing(&variant.pricing).map_err(|e| {
                format!("items[{idx}]: promptWriteCacheVariants[{vidx}]: pricing: {e}")
            })?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_pricing_change_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor_user_id: Option<i64>,
    operation: &str,
    entity_type: &str,
    entity_id: &str,
    before_snapshot: Option<serde_json::Value>,
    after_snapshot: Option<serde_json::Value>,
    accounting_currency: &str,
    accounting_settings_version: u64,
    request_correlation_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO pricing_change_audits \
         (actor_type,actor_id,operation,entity_type,entity_id,before_snapshot,after_snapshot, \
          accounting_currency,accounting_settings_version,result,request_correlation_id,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'success',$10,$11)",
    )
    .bind(if actor_user_id.is_some() {
        "user"
    } else {
        "system"
    })
    .bind(actor_user_id)
    .bind(operation)
    .bind(entity_type)
    .bind(entity_id)
    .bind(before_snapshot)
    .bind(after_snapshot)
    .bind(accounting_currency)
    .bind(i64::try_from(accounting_settings_version).unwrap_or(i64::MAX))
    .bind(request_correlation_id)
    .bind(chrono::Utc::now())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Normalize one explicit price-row currency without consulting mutable system
/// settings. A saved numeric price and its currency label form one indivisible
/// value; callers must provide both on every create/update.
pub(crate) fn normalize_price_currency_code(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.len() == 3
        && normalized
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        Ok(normalized)
    } else {
        Err("currencyCode must be a 3-letter ISO currency code".to_string())
    }
}

/// One planned mutation against the price head + version tables. Mirrors Go
/// `PriceChangeAction` (`biz/channel_price.go:32-37`) — the `Update`/`Delete`
/// arms carry the existing head row (Go `ExistingPrice`).
#[derive(Debug)]
pub(crate) enum PriceAction {
    Skip,
    Delete {
        existing: conduit_db::row::ChannelModelPriceRow,
    },
    Create {
        model_id: String,
        price_json: serde_json::Value,
    },
    Update {
        existing: conduit_db::row::ChannelModelPriceRow,
        model_id: String,
        price_json: serde_json::Value,
    },
}

/// Port of Go `calculatePriceChanges` (`biz/channel_price.go:39-94`).
///
/// Iterates the inputs in order (deterministic action ordering): each input is
/// a Create (no existing row for the model), a Skip (existing price equals the
/// input price), or an Update. Then appends a Delete for every existing row
/// whose model id is absent from the input set. Price equality mirrors Go
/// `ModelPrice.Equals` via structural `PartialEq` on the deserialized price.
pub(crate) fn calculate_price_changes(
    existing: &[conduit_db::row::ChannelModelPriceRow],
    inputs: &[(String, String, core_pricing::ModelPrice, serde_json::Value)],
) -> Vec<PriceAction> {
    use std::collections::HashMap;

    let existing_by_model: HashMap<&str, &conduit_db::row::ChannelModelPriceRow> = existing
        .iter()
        .map(|row| (row.model_id.as_str(), row))
        .collect();

    let mut input_models: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut actions: Vec<PriceAction> = Vec::new();

    // 1) Creates / updates / skips, in input order.
    for (model_id, currency_code, core_price, price_json) in inputs.iter() {
        input_models.insert(model_id.as_str());
        match existing_by_model.get(model_id.as_str()) {
            None => actions.push(PriceAction::Create {
                model_id: model_id.clone(),
                price_json: price_json.clone(),
            }),
            Some(row) => {
                // Structural equality of the deserialized prices (Go
                // `existing.Price.Equals(input.Price)`). A stored price that
                // fails to deserialize is treated as "changed" (never skip).
                let stored: Option<core_pricing::ModelPrice> =
                    serde_json::from_value(row.price.clone()).ok();
                if stored.as_ref() == Some(core_price)
                    && row.currency_code.eq_ignore_ascii_case(currency_code)
                {
                    actions.push(PriceAction::Skip);
                } else {
                    actions.push(PriceAction::Update {
                        existing: (*row).clone(),
                        model_id: model_id.clone(),
                        price_json: price_json.clone(),
                    });
                }
            }
        }
    }

    // 2) Deletes: existing rows whose model is not in the input set.
    for row in existing.iter() {
        if !input_models.contains(row.model_id.as_str()) {
            actions.push(PriceAction::Delete {
                existing: row.clone(),
            });
        }
    }

    actions
}

#[cfg(test)]
mod channel_price_change_tests {
    use super::*;

    fn row(
        model_id: &str,
        currency_code: &str,
        price: &core_pricing::ModelPrice,
    ) -> conduit_db::row::ChannelModelPriceRow {
        let now = chrono::Utc::now();
        conduit_db::row::ChannelModelPriceRow {
            id: model_id.to_string(),
            channel_id: "1".to_string(),
            model_id: model_id.to_string(),
            currency_code: currency_code.to_string(),
            price: serde_json::to_value(price).expect("price serializes"),
            reference_id: format!("ref-{model_id}"),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    #[test]
    fn explicit_price_currency_is_normalized_and_validated() {
        assert_eq!(normalize_price_currency_code(" usd ").as_deref(), Ok("USD"));
        assert!(normalize_price_currency_code("US").is_err());
        assert!(normalize_price_currency_code("US1").is_err());
    }

    #[test]
    fn price_validation_rejects_negative_amounts_and_invalid_tier_boundaries() {
        let negative_usage = core_pricing::Pricing {
            mode: core_pricing::PRICING_MODE_USAGE_PER_UNIT.into(),
            usage_per_unit: Some(rust_decimal::Decimal::new(-1, 0)),
            ..Default::default()
        };
        assert_eq!(
            validate_pricing(&negative_usage).unwrap_err(),
            "usagePerUnit must not be negative"
        );

        let invalid_tiers = core_pricing::Pricing {
            mode: core_pricing::PRICING_MODE_TIERED.into(),
            usage_tiered: Some(core_pricing::TieredPricing {
                tiers: vec![
                    core_pricing::PriceTier {
                        up_to: Some(100),
                        price_per_unit: rust_decimal::Decimal::ONE,
                    },
                    core_pricing::PriceTier {
                        up_to: Some(50),
                        price_per_unit: rust_decimal::Decimal::ONE,
                    },
                    core_pricing::PriceTier {
                        up_to: None,
                        price_per_unit: rust_decimal::Decimal::ONE,
                    },
                ],
            }),
            ..Default::default()
        };
        assert_eq!(
            validate_pricing(&invalid_tiers).unwrap_err(),
            "tiers[1].upTo must be greater than 100"
        );

        let zero_usage = core_pricing::Pricing {
            mode: core_pricing::PRICING_MODE_USAGE_PER_UNIT.into(),
            usage_per_unit: Some(rust_decimal::Decimal::ZERO),
            ..Default::default()
        };
        assert!(validate_pricing(&zero_usage).is_ok());
    }

    #[test]
    fn price_change_plan_detects_skips_updates_and_creates() {
        let unchanged = core_pricing::ModelPrice::default();
        let changed = core_pricing::ModelPrice {
            items: vec![core_pricing::ModelPriceItem::default()],
        };
        let existing = vec![
            row("model-a", "CNY", &unchanged),
            row("model-b", "CNY", &unchanged),
        ];
        let inputs = vec![
            (
                "model-a".to_string(),
                "CNY".to_string(),
                unchanged.clone(),
                serde_json::to_value(&unchanged).expect("price serializes"),
            ),
            (
                "model-b".to_string(),
                "CNY".to_string(),
                changed.clone(),
                serde_json::to_value(&changed).expect("price serializes"),
            ),
            (
                "model-c".to_string(),
                "CNY".to_string(),
                unchanged.clone(),
                serde_json::to_value(&unchanged).expect("price serializes"),
            ),
        ];

        let actions = calculate_price_changes(&existing, &inputs);
        assert_eq!(actions.len(), 3);
        match &actions[0] {
            PriceAction::Skip => {}
            other => panic!("unchanged row should skip, got {other:?}"),
        }
        match &actions[1] {
            PriceAction::Update { .. } => {}
            other => panic!("changed row should update, got {other:?}"),
        }
        match &actions[2] {
            PriceAction::Create { .. } => {}
            other => panic!("new row should create, got {other:?}"),
        }
    }
}

/// Port of Go `generateReferenceID` (`biz/channel_price.go:270-280`): an
/// 8-character `[a-zA-Z]` string. The workspace has no `rand` crate, so the
/// randomness comes from a per-process atomic counter mixed with the current
/// nanosecond clock and hashed — collisions are made astronomically unlikely
/// and the shape (`len==8`, alphabetic) matches Go. The `reference_id` column
/// is UNIQUE, so a (theoretical) collision surfaces as an insert error rather
/// than silent corruption.
pub(crate) fn generate_reference_id() -> String {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let mut out = String::with_capacity(8);
    let mut state = seq.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(nanos);
    for _ in 0..8 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        state.hash(&mut hasher);
        state = hasher.finish().wrapping_add(0x0123_4567_89AB_CDEF);
        let idx = (state % LETTERS.len() as u64) as usize;
        out.push(LETTERS[idx] as char);
    }
    out
}

/// Convert a stored `ChannelModelPriceRow` into the GraphQL `ChannelModelPrice`
/// output object. The `price` JSON column is deserialized through the core
/// `objects::ModelPrice` (zero-filling missing fields, mirroring Ent's
/// value-type marshalling) and then mapped to the GraphQL price object.
fn price_row_to_gql(
    row: conduit_db::row::ChannelModelPriceRow,
) -> Result<conduit_admin_graphql::model_ext::ChannelModelPrice, ModelExtError> {
    use conduit_admin_graphql::model_ext::ChannelModelPrice as GqlChannelModelPrice;
    use conduit_admin_graphql::scalars::TimeScalar;

    let core_price: core_pricing::ModelPrice =
        serde_json::from_value(row.price).unwrap_or_default();
    Ok(GqlChannelModelPrice {
        id: format!("gid://conduit/ChannelModelPrice/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        channel_id: format!("gid://conduit/Channel/{}", row.channel_id).into(),
        model_id: row.model_id,
        currency_code: row.currency_code,
        price: crate::conv::model_price_core_to_gql(core_price),
        reference_id: row.reference_id,
    })
}

impl ModelExtAdapter {
    fn new(
        model_repo: Arc<dyn ModelRepo>,
        channel_repo: Arc<dyn ChannelRepo>,
        system: Arc<DomainSystemService>,
        pool: PgPool,
    ) -> Self {
        Self {
            model_repo,
            channel_repo,
            system,
            pool,
        }
    }

    /// Shared body for the three bulk status setters + `update_model_status`:
    /// update each id's `status` column via `ModelRepo::update_model`. Mirrors
    /// Go `Model.Update().Where(IDIn(ids)).SetStatus(status)` (one row at a
    /// time, since the repo has no batch update). Any single failure aborts and
    /// surfaces via `wrap`.
    async fn set_status_for_ids(
        &self,
        ids: &[async_graphql::ID],
        status_wire: &str,
        wrap: impl Fn(String) -> ModelExtError,
    ) -> Result<(), ModelExtError> {
        let ctx = boot_request_context();
        let now = chrono::Utc::now().to_rfc3339();
        for id in ids {
            let db_id = model_id_from_gql(id)?;
            let input = UpdateModelInput {
                status: Some(status_wire.to_string()),
                updated_at: now.clone(),
                ..UpdateModelInput::default()
            };
            self.model_repo
                .update_model(&ctx, &db_id, input)
                .await
                .map_err(|e| wrap(e.to_string()))?;
        }
        Ok(())
    }

    /// Load every enabled+disabled channel ordered by `ordering_weight` desc,
    /// mirroring Go's channel query in `QueryModelChannelConnections` /
    /// `QueryUnassociatedChannels`
    /// (`Channel.Query().Where(StatusIn(enabled,disabled)).Order(ByOrderingWeight(desc))`).
    /// Pages the repo in generous windows (the channels table is small) — same
    /// shape as `ChannelExtraQueryAdapter::load_all`. On error returns the
    /// wrapped repo string for the caller to fit into its `ModelExtError`.
    async fn load_channels_for_matching(&self) -> Result<Vec<conduit_db::row::ChannelRow>, String> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListChannelsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
                status_in: vec!["enabled".to_string(), "disabled".to_string()],
            };
            let result = self
                .channel_repo
                .list_channels(&ctx, &query)
                .await
                .map_err(|e| e.to_string())?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
        }
        // The repo returns created_at-asc order; Go orders by ordering_weight
        // desc (stable on ties).
        rows.sort_by(|a, b| b.ordering_weight.cmp(&a.ordering_weight));
        Ok(rows)
    }
}

#[async_trait]
impl ModelExtServices for ModelExtAdapter {
    async fn fetch_models(
        &self,
        input: conduit_admin_graphql::model_ext::FetchModelsInput,
    ) -> Result<conduit_admin_graphql::model_ext::FetchModelsPayload, ModelExtError> {
        use conduit_admin_graphql::model_ext::{FetchModelsPayload, ModelIdentify};

        // Resolve the API key + effective channel_type/base_url. Explicit
        // `input.api_key` wins; otherwise fall back to the referenced channel's
        // first credential (Go `FetchModels`: reads `ch.Credentials.APIKey` /
        // `APIKeys[0]` and the channel's type/base_url when the key is omitted).
        let mut channel_type = input.channel_type.clone();
        let mut base_url = input.base_url.clone();
        let mut api_key = input.api_key.clone().unwrap_or_default();

        if api_key.is_empty()
            && let Some(id) = input.channel_id.as_ref()
        {
            // Accept either a `gid://conduit/Channel/<n>` GUID or a bare
            // numeric id.
            let db_id = parse_guid(id.as_str())
                .ok()
                .map(|g| g.id.to_string())
                .or_else(|| id.as_str().parse::<i64>().ok().map(|n| n.to_string()));
            if let Some(db_id) = db_id {
                let ctx = boot_request_context();
                if let Ok(Some(row)) = self.channel_repo.find_channel(&ctx, &db_id).await {
                    let creds: conduit_core::objects::channel_settings::ChannelCredentials =
                        serde_json::from_value(row.credentials.clone()).unwrap_or_default();
                    if let Some(keys) = creds.get_all_api_keys()
                        && let Some(first) = keys.into_iter().next()
                    {
                        api_key = first;
                    }
                    // When the key came from the channel, adopt the
                    // channel's type/base_url too (Go's same branch).
                    channel_type = row.channel_type.clone();
                    base_url = row.base_url.clone().unwrap_or(base_url);
                }
            }
        }

        let client = reqwest::Client::new();
        let fetched =
            crate::model_fetch::fetch_models(&client, &channel_type, &base_url, &api_key).await;

        Ok(FetchModelsPayload {
            models: fetched
                .model_ids
                .into_iter()
                .map(|id| ModelIdentify { id })
                .collect(),
            error: fetched.error,
        })
    }

    async fn query_models(
        &self,
        input: conduit_admin_graphql::model_ext::QueryModelsInput,
    ) -> Result<Vec<GqlModelIdentityWithStatus>, ModelExtError> {
        let ctx = boot_request_context();

        // Configured-models path: list DB models, filter by status.
        let query = ListModelsQuery {
            limit: 1000,
            offset: 0,
            after_created_at: None,
            after_id: None,
        };
        let result = self
            .model_repo
            .list_models(&ctx, &query)
            .await
            .map_err(|e| ModelExtError::QueryModels(e.to_string()))?;

        // Requested status set → domain wire strings. Empty/None → ["enabled"]
        // (Go default).
        let wanted: Vec<&'static str> = match &input.status_in {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|s| match s {
                    GqlChannelStatus::Enabled => "enabled",
                    GqlChannelStatus::Disabled => "disabled",
                    GqlChannelStatus::Archived => "archived",
                })
                .collect(),
            _ => vec!["enabled"],
        };

        let configured_models: Vec<_> = result
            .rows
            .into_iter()
            .filter(|m| wanted.contains(&m.status.as_str()))
            .map(|m| GqlModelIdentityWithStatus {
                id: m.model_id,
                status: model_status_str_to_channel_status(&m.status),
            })
            .collect();

        // If `includeAllChannelModels=false` (default), return configured models only.
        if !input.include_all_channel_models.unwrap_or(false) {
            return Ok(configured_models);
        }

        // `includeAllChannelModels=true`: merge configured models (higher priority)
        // with channel-derived models (Go `modelService.ListEnabledModels`).
        let mut model_set: std::collections::HashSet<String> =
            configured_models.iter().map(|m| m.id.clone()).collect();

        // Load enabled channels (Go filters `.Where(StatusIn(enabled,disabled))`).
        let channel_query = ListChannelsQuery {
            limit: 1000,
            offset: 0,
            after_created_at: None,
            after_id: None,
            status_in: vec!["enabled".to_string(), "disabled".to_string()],
        };
        let channels = self
            .channel_repo
            .list_channels(&ctx, &channel_query)
            .await
            .map_err(|e| ModelExtError::QueryModels(e.to_string()))?;

        // Get ModelBlacklistRegex from system settings (Go `settings.ModelBlacklistRegex`).
        let system_settings = self.system.model_settings(&ctx).await.unwrap_or_default();
        let blacklist_regex = &system_settings.model_blacklist_regex;
        let blacklist_re = if !blacklist_regex.is_empty() {
            regex::Regex::new(blacklist_regex).ok()
        } else {
            None
        };

        let mut channel_models: Vec<GqlModelIdentityWithStatus> = Vec::new();

        for ch in channels.rows {
            // Parse settings + build model entry map (Go `ch.GetModelEntries()`).
            let settings: conduit_core::objects::channel_settings::ChannelSettings =
                serde_json::from_value(ch.settings).unwrap_or_default();
            let entry_map =
                crate::model_matcher::build_model_entries(&ch.supported_models, &settings);

            for request_model in entry_map.keys() {
                // Skip if already in model_set (configured models take precedence).
                if model_set.contains(request_model) {
                    continue;
                }

                // Apply blacklist regex to channel-derived models only
                // (configured models are immune).
                if let Some(ref re) = blacklist_re
                    && re.is_match(request_model)
                {
                    // Cache the decision so the same model from another channel
                    // skips the regex match (Go does `modelSet[requestModel] = true`
                    // even for blacklisted entries to short-circuit the regex).
                    model_set.insert(request_model.clone());
                    continue;
                }

                model_set.insert(request_model.clone());
                channel_models.push(GqlModelIdentityWithStatus {
                    id: request_model.clone(),
                    // Channel-derived models inherit the channel's status
                    // (Go returns them as enabled if the channel is enabled).
                    status: model_status_str_to_channel_status(&ch.status),
                });
            }
        }

        // Merge configured + channel-derived (Go: `models = append(configuredModels, ...)`).
        let mut all_models = configured_models;
        all_models.extend(channel_models);
        Ok(all_models)
    }

    async fn query_model_channel_connections(
        &self,
        associations: Vec<conduit_admin_graphql::model::ModelAssociationInput>,
    ) -> Result<Vec<conduit_admin_graphql::model_ext::ModelChannelConnection>, ModelExtError> {
        // Go `QueryModelChannelConnections` (`biz/model.go:521-545`): empty
        // associations → empty result (no channel query at all).
        if associations.is_empty() {
            return Ok(Vec::new());
        }

        // Lower the GraphQL association inputs into the core typed shape the
        // matcher consumes (reuses the CONV-01 input converter).
        let core_assocs: Vec<conduit_core::objects::ModelAssociation> = associations
            .iter()
            .map(crate::conv::model_association_input_to_core)
            .collect();

        // Load enabled+disabled channels ordered by ordering_weight desc, matching
        // Go's `.Where(StatusIn(enabled,disabled)).Order(ByOrderingWeight(desc))`.
        let rows = self
            .load_channels_for_matching()
            .await
            .map_err(ModelExtError::QueryModelChannelConnections)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        Ok(crate::model_matcher::resolve_model_channel_connections(
            rows,
            &core_assocs,
        ))
    }

    async fn query_unassociated_channels(
        &self,
    ) -> Result<Vec<conduit_admin_graphql::model_ext::UnassociatedChannel>, ModelExtError> {
        // Go `QueryUnassociatedChannels` (`biz/model.go:745-772`): load
        // enabled+disabled channels; empty → empty. Otherwise gather every
        // model's effective associations and diff against the channels' models.
        let rows = self
            .load_channels_for_matching()
            .await
            .map_err(ModelExtError::QueryUnassociatedChannels)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // Load enabled+disabled models (Go: `Model.Query().Where(StatusIn(...))`).
        let ctx = boot_request_context();
        let query = ListModelsQuery {
            limit: 1000,
            offset: 0,
            after_created_at: None,
            after_id: None,
        };
        let model_rows = self
            .model_repo
            .list_models(&ctx, &query)
            .await
            .map_err(|e| ModelExtError::QueryUnassociatedChannels(e.to_string()))?
            .rows
            .into_iter()
            .filter(|m| m.status == "enabled" || m.status == "disabled")
            .collect::<Vec<_>>();

        // System model settings drive developer-association inheritance (Go
        // `modelSettingsOrDefault`); fall back to defaults if the read fails.
        let system_settings = self.system.model_settings(&ctx).await.unwrap_or_default();

        let associations =
            crate::model_matcher::effective_associations_for_models(&model_rows, &system_settings);

        Ok(crate::model_matcher::resolve_unassociated_channels(
            rows,
            &associations,
        ))
    }

    async fn bulk_create_models(
        &self,
        inputs: Vec<conduit_admin_graphql::model::CreateModelInput>,
    ) -> Result<Vec<conduit_admin_graphql::model::Model>, ModelExtError> {
        // Mirrors Go `BulkCreateModels` (`model.resolvers.go:31-34`) →
        // `modelService.BulkCreateModels`: create each model and return the
        // created rows. Go runs these in one ent bulk-create; the Rust repo has
        // no batch surface, so we `create_model` per input (semantically
        // identical for independent rows). Any single failure aborts the batch
        // and surfaces the repo error — the CONV-01 converters
        // (`create_model_columns` + `model_row_to_gql`) bridge the typed
        // card/settings <-> JSON columns.
        let ctx = boot_request_context();
        let now = chrono::Utc::now().to_rfc3339();
        let mut created = Vec::with_capacity(inputs.len());
        for input in inputs {
            let cols = crate::conv::create_model_columns(&input);
            let repo_input = RepoCreateModelInput {
                // PostgreSQL owns the generated PK; the `id` here is
                // ignored on insert (read-back uses the DB id).
                id: String::new(),
                developer: input.developer,
                model_id: input.model_id,
                name: input.name,
                model_type: Some(cols.model_type),
                icon: Some(input.icon),
                group: input.group,
                model_card: Some(cols.model_card),
                settings: Some(cols.settings),
                remark: input.remark,
                created_at: now.clone(),
            };
            let row = self
                .model_repo
                .create_model(&ctx, repo_input)
                .await
                .map_err(|e| ModelExtError::BulkCreateModels(e.to_string()))?;
            created.push(crate::conv::model_row_to_gql(row));
        }
        Ok(created)
    }

    async fn update_model_status(
        &self,
        id: async_graphql::ID,
        status: GqlModelStatus,
    ) -> Result<(), ModelExtError> {
        self.set_status_for_ids(
            std::slice::from_ref(&id),
            model_status_to_wire(status),
            ModelExtError::UpdateModelStatus,
        )
        .await
    }

    async fn bulk_archive_models(&self, ids: Vec<async_graphql::ID>) -> Result<(), ModelExtError> {
        self.set_status_for_ids(&ids, "archived", ModelExtError::BulkArchiveModels)
            .await
    }

    async fn bulk_disable_models(&self, ids: Vec<async_graphql::ID>) -> Result<(), ModelExtError> {
        self.set_status_for_ids(&ids, "disabled", ModelExtError::BulkDisableModels)
            .await
    }

    async fn bulk_enable_models(&self, ids: Vec<async_graphql::ID>) -> Result<(), ModelExtError> {
        self.set_status_for_ids(&ids, "enabled", ModelExtError::BulkEnableModels)
            .await
    }

    async fn bulk_delete_models(&self, ids: Vec<async_graphql::ID>) -> Result<(), ModelExtError> {
        // Go hard-deletes (`Model.Delete().Where(IDIn)`); the Rust repo only
        // exposes soft delete. Soft-delete each id — hides the row from every
        // default query, the effect the admin UI observes. Documented
        // divergence (see the adapter doc comment).
        let ctx = boot_request_context();
        let now = chrono::Utc::now().to_rfc3339();
        for id in &ids {
            let db_id = model_id_from_gql(id)?;
            self.model_repo
                .soft_delete_model(&ctx, &db_id, &now)
                .await
                .map_err(|e| ModelExtError::BulkDeleteModels(e.to_string()))?;
        }
        Ok(())
    }

    async fn save_channel_model_prices(
        &self,
        actor_user_id: Option<i64>,
        channel_id: async_graphql::ID,
        input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<Vec<conduit_admin_graphql::model_ext::ChannelModelPrice>, ModelExtError> {
        // Compatibility entry point: manual edits now stage a unified draft.
        // Formal prices are only changed by ChangeSet approval.
        let staging_actor_user_id = actor_user_id.ok_or_else(|| {
            ModelExtError::SaveChannelModelPrices("authentication required".into())
        })?;
        let change_sets =
            crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(self.pool.clone());
        conduit_admin_graphql::change_set::ChangeSetServices::create_provider_price_change_set(
            &change_sets,
            staging_actor_user_id,
            channel_id.clone(),
            input,
        )
        .await
        .map_err(|error| ModelExtError::SaveChannelModelPrices(error.to_string()))?;

        let channel_db = channel_price_db_id(channel_id.as_str()).ok_or_else(|| {
            ModelExtError::SaveChannelModelPrices(format!(
                "invalid channel id: {}",
                channel_id.as_str()
            ))
        })?;
        let price_repo = conduit_db::PgChannelModelPriceRepo::new(self.pool.clone());
        price_repo
            .list_prices_by_channel(&boot_request_context(), channel_db)
            .await
            .map_err(|error| ModelExtError::SaveChannelModelPrices(error.to_string()))?
            .into_iter()
            .map(price_row_to_gql)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Base model CRUD adapter (P12-001 S07) — backs the admin GraphQL `models`
// connection query + `createModel` / `updateModel` / `deleteModel` mutations.
// Mirrors Go `Query.models` (`ent.resolvers.go:371`, `r.client.Model.Query().
// Paginate`) and `biz.ModelService.{CreateModel,UpdateModel,DeleteModel}`
// (`biz/model.go:311/408/462`).
//
// ## Boundary with `ModelExtAdapter`
//
// The two adapters partition the model domain with NO method overlap. This one
// owns `ModelQueryServices` (the `models` Relay connection) +
// `ModelMutationServices` (create/update/delete). `ModelExtAdapter` above owns
// `ModelExtServices` — a DIFFERENT trait — for the extended surface
// (`queryModels`, `updateModelStatus`, `bulk*`). Both delegate to the same live
// the model repository; the CRUD path here reuses the CONV-01 converters
// (`model_row_to_gql` for reads, `create_model_columns` /
// `model_{card,settings}_input_to_json` for writes) exactly like
// `ModelExtAdapter::bulk_create_models`.
//
// ## Deliberate divergences (documented)
//
//   - Bounded-table strategy: the `models` connection materializes the full
//     (non-deleted) row set from the repo and applies the `where` filter,
//     ordering, and Relay `after`/`first` pagination in memory — the same
//     approach `ChannelExtraQueryAdapter` uses. Backward pagination
//     (`before`/`last`) is not applied (the admin model list page drives
//     forward paging only); a future streaming path would push these down.
//   - `where` predicate coverage: the equality / `in` / `neq` / `contains` /
//     `hasPrefix` / `hasSuffix` families plus `not`/`and`/`or` and the enum +
//     `remark` nil predicates are lowered. The ordering-comparison predicates
//     (`GT`/`GTE`/`LT`/`LTE`), the case-fold variants, and the `id` predicates
//     are treated as no-ops (not yet lowered) — same bounded divergence the
//     channels adapter accepts.
//   - `deleteModel`: Go hard-deletes (`Model.DeleteOneID`); the repo only
//     exposes `soft_delete_model` (sets `deleted_at`, hides the row from every
//     default query — the effect the admin UI observes). Documented divergence,
//     identical to `ModelExtAdapter::bulk_delete_models`.
//   - Settings regex validation (Go `validateModelSettings`) lives in
//     `conduit-services` and is not repo-backed here, so create/update skip it —
//     same as `ModelExtAdapter::bulk_create_models`.
// ---------------------------------------------------------------------------

/// Host implementation for the derived `Model.associatedChannelCount` field.
/// It deliberately lives outside the CRUD adapter so every GraphQL path that
/// produces a Model (connection, mutation, bulk mutation, or Node lookup) uses
/// the same live calculation.
struct ModelAssociationCountAdapter {
    channel_repo: Arc<dyn ChannelRepo>,
    system: Arc<DomainSystemService>,
}

impl ModelAssociationCountAdapter {
    fn new(channel_repo: Arc<dyn ChannelRepo>, system: Arc<DomainSystemService>) -> Self {
        Self {
            channel_repo,
            system,
        }
    }

    async fn load_channels(&self) -> Result<Vec<conduit_db::row::ChannelRow>, ModelServiceError> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListChannelsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
                status_in: vec!["enabled".to_string(), "disabled".to_string()],
            };
            let result = self
                .channel_repo
                .list_channels(&ctx, &query)
                .await
                .map_err(|error| ModelServiceError::Query(error.to_string()))?;
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
impl ModelAssociationCountServices for ModelAssociationCountAdapter {
    async fn associated_channel_count(&self, model: &GqlModel) -> Result<i64, ModelServiceError> {
        let model_settings = conduit_core::objects::ModelSettings {
            disable_developer_settings_inheritance: model
                .settings
                .disable_developer_settings_inheritance,
            associations: model
                .settings
                .associations
                .clone()
                .into_iter()
                .map(crate::conv::model_association_to_core)
                .collect(),
        };
        let ctx = boot_request_context();
        let system_settings = self.system.model_settings(&ctx).await.unwrap_or_default();
        let associations = conduit_services::model_service::effective_model_associations(
            &system_settings,
            &model.developer,
            &model.model_id,
            Some(&model_settings),
        );
        if associations.is_empty() {
            return Ok(0);
        }

        let count = crate::model_matcher::count_associated_channels(
            self.load_channels().await?,
            &associations,
        );
        Ok(i64::try_from(count).unwrap_or(i64::MAX))
    }
}

/// GraphQL-facing [`ModelQueryServices`] + [`ModelMutationServices`] adapter
/// backed by the live [`ModelRepo`].
struct ModelCrudAdapter {
    model_repo: Arc<dyn ModelRepo>,
}

/// GraphQL `ModelType` enum → the Go wire literal stored in the `type` column.
/// Matches the `#[graphql(name = ...)]` spellings on the enum (and the private
/// `model_type_to_wire` in `crate::conv`).
fn gql_model_type_to_wire(t: GqlModelType) -> &'static str {
    match t {
        GqlModelType::Chat => "chat",
        GqlModelType::Embedding => "embedding",
        GqlModelType::Rerank => "rerank",
        GqlModelType::ImageGeneration => "image_generation",
        GqlModelType::VideoGeneration => "video_generation",
    }
}

/// Map a GraphQL `ID` string (a `gid://conduit/Model/<n>` wire form or a bare
/// numeric id) to the numeric DB id string the repo expects. Mirrors Go
/// `GUID.UnmarshalGQL` accepting the typed GUID form; a value that is neither is
/// treated as "no such model" (Go would reject it at argument parsing).
fn model_db_id_from_gql_str(raw: &str) -> Result<String, ModelServiceError> {
    if let Ok(guid) = parse_guid(raw) {
        return Ok(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Ok(raw.to_string());
    }
    Err(ModelServiceError::NotFound)
}

/// Evaluate the string-field predicate family (eq / neq / in / notIn /
/// contains / hasPrefix / hasSuffix) against a column value. Any predicate that
/// is `None` is skipped (AND semantics across the set, matching ent).
#[allow(clippy::too_many_arguments)]
fn str_field_matches(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    contains: &Option<String>,
    has_prefix: &Option<String>,
    has_suffix: &Option<String>,
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
    if let Some(list) = in_set
        && !list.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|x| x == value)
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
    true
}

/// Whether a `ModelRow` satisfies a `ModelWhereInput` predicate tree. See the
/// adapter doc for the covered vs. deferred predicate families. `not`/`and`/`or`
/// recurse; an empty `and` matches (ent semantics) and an empty `or` is ignored
/// so it never blacks out the result.
fn model_row_matches_where(row: &conduit_db::row::ModelRow, w: &GqlModelWhereInput) -> bool {
    if let Some(inner) = &w.not
        && model_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| model_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| model_row_matches_where(row, c))
    {
        return false;
    }

    if !str_field_matches(
        &row.developer,
        &w.developer,
        &w.developer_neq,
        &w.developer_in,
        &w.developer_not_in,
        &w.developer_contains,
        &w.developer_has_prefix,
        &w.developer_has_suffix,
    ) {
        return false;
    }
    if !str_field_matches(
        &row.model_id,
        &w.model_id,
        &w.model_id_neq,
        &w.model_id_in,
        &w.model_id_not_in,
        &w.model_id_contains,
        &w.model_id_has_prefix,
        &w.model_id_has_suffix,
    ) {
        return false;
    }
    if !str_field_matches(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
    ) {
        return false;
    }
    if !str_field_matches(
        &row.icon,
        &w.icon,
        &w.icon_neq,
        &w.icon_in,
        &w.icon_not_in,
        &w.icon_contains,
        &w.icon_has_prefix,
        &w.icon_has_suffix,
    ) {
        return false;
    }
    if !str_field_matches(
        &row.group_name,
        &w.group,
        &w.group_neq,
        &w.group_in,
        &w.group_not_in,
        &w.group_contains,
        &w.group_has_prefix,
        &w.group_has_suffix,
    ) {
        return false;
    }

    // status enum predicates
    if let Some(s) = w.status
        && row.status != model_status_to_wire(s)
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row.status == model_status_to_wire(s)
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list.iter().any(|s| row.status == model_status_to_wire(*s))
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list.iter().any(|s| row.status == model_status_to_wire(*s))
    {
        return false;
    }

    // type enum predicates
    if let Some(t) = w.model_type
        && row.model_type != gql_model_type_to_wire(t)
    {
        return false;
    }
    if let Some(t) = w.type_neq
        && row.model_type == gql_model_type_to_wire(t)
    {
        return false;
    }
    if let Some(list) = &w.type_in
        && !list
            .iter()
            .any(|t| row.model_type == gql_model_type_to_wire(*t))
    {
        return false;
    }
    if let Some(list) = &w.type_not_in
        && list
            .iter()
            .any(|t| row.model_type == gql_model_type_to_wire(*t))
    {
        return false;
    }

    // remark nil predicates + string family (a NULL remark reads as "").
    if w.remark_is_nil == Some(true) && row.remark.is_some() {
        return false;
    }
    if w.remark_not_nil == Some(true) && row.remark.is_none() {
        return false;
    }
    let remark = row.remark.as_deref().unwrap_or("");
    if !str_field_matches(
        remark,
        &w.remark,
        &w.remark_neq,
        &w.remark_in,
        &w.remark_not_in,
        &w.remark_contains,
        &w.remark_has_prefix,
        &w.remark_has_suffix,
    ) {
        return false;
    }

    true
}

impl ModelCrudAdapter {
    fn new(model_repo: Arc<dyn ModelRepo>) -> Self {
        Self { model_repo }
    }

    /// Materialize every live (non-deleted) model row, paging through the repo
    /// in generous windows. The models table is small (a gateway has tens to
    /// low-hundreds of models), so a full in-memory load faithfully mirrors Go's
    /// ent `.All(ctx)` without a streaming cursor.
    async fn load_all(&self) -> Result<Vec<conduit_db::row::ModelRow>, ModelServiceError> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListModelsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
            };
            let result = self
                .model_repo
                .list_models(&ctx, &query)
                .await
                .map_err(|e| ModelServiceError::Query(e.to_string()))?;
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
impl ModelQueryServices for ModelCrudAdapter {
    async fn models(
        &self,
        args: ModelConnectionArgs,
    ) -> Result<GqlModelConnection, ModelServiceError> {
        use conduit_admin_graphql::channel::OrderDirection;
        use conduit_admin_graphql::pagination::{
            connection_from_offset_page, decode_offset_cursor,
        };
        use conduit_admin_graphql::scalars::CursorScalar;

        let rows = self.load_all().await?;

        // `where` filter (in-memory; see adapter doc for covered predicates).
        let mut rows: Vec<conduit_db::row::ModelRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| model_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Ordering: the crate already lowered `CREATED_AT` → `Id` (ent
        // DefaultModelOrder). The repo returns created_at-asc, so re-sort for
        // any explicit selection.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    ModelOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    ModelOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                    ModelOrderTerm::Name => a.name.cmp(&b.name),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let models: Vec<GqlModel> = rows
            .into_iter()
            .map(crate::conv::model_row_to_gql)
            .collect();

        // Relay forward pagination over the offset-cursor scheme (matching
        // `connection_from_offset_page`). A malformed `after` degrades to
        // offset 0 rather than failing the whole query.
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset).unwrap_or(0).min(models.len());
        let windowed = models[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

        Ok(GqlModelConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(GqlModelEdge {
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
impl ModelMutationServices for ModelCrudAdapter {
    async fn create_model(
        &self,
        input: GqlCreateModelInput,
    ) -> Result<GqlModel, ModelServiceError> {
        let ctx = boot_request_context();

        // Go CreateModel (biz/model.go:319-329): the duplicate probe queries
        // `model.ModelID(input.ModelID)` ONLY and fails with
        // `xerrors.DuplicateNameError("model", input.ModelID)`. We probe first so
        // a modelID collision surfaces that message; a `name` collision (also a
        // unique index in Go) falls through to the repo's insert and surfaces as
        // a wrapped "failed to create model" (Go's ent Save error path).
        let existing = self
            .model_repo
            .find_model_by_model_id(&ctx, &input.model_id)
            .await
            .map_err(|e| ModelServiceError::Create(e.to_string()))?;
        if existing.is_some() {
            return Err(ModelServiceError::DuplicateName(input.model_id));
        }

        // CONV-01 write path: lower the typed card/settings inputs to the JSON
        // columns + resolve the `type` default (Go column default `chat`); the
        // repo inserts with `status = 'disabled'` (Go column default, and
        // CreateModelInput has no status field — `SkipMutationCreateInput`).
        let cols = crate::conv::create_model_columns(&input);
        let now = chrono::Utc::now().to_rfc3339();
        let repo_input = RepoCreateModelInput {
            // PostgreSQL owns the generated PK; `id` is ignored on
            // insert (read-back uses the DB id).
            id: String::new(),
            developer: input.developer,
            model_id: input.model_id,
            name: input.name,
            model_type: Some(cols.model_type),
            icon: Some(input.icon),
            group: input.group,
            model_card: Some(cols.model_card),
            settings: Some(cols.settings),
            remark: input.remark,
            created_at: now,
        };
        let row = self
            .model_repo
            .create_model(&ctx, repo_input)
            .await
            .map_err(|e| ModelServiceError::Create(e.to_string()))?;
        Ok(crate::conv::model_row_to_gql(row))
    }

    async fn update_model(
        &self,
        id: &str,
        input: GqlUpdateModelInput,
    ) -> Result<GqlModel, ModelServiceError> {
        let ctx = boot_request_context();
        let db_id = model_db_id_from_gql_str(id)?;
        let now = chrono::Utc::now().to_rfc3339();

        // Field application mirrors Go UpdateModel (biz/model.go:416-439):
        // SetNillable{Developer,ModelID,Type,Name,Group,Status,Icon}; conditional
        // Set for modelCard/settings; SetRemark then ClearRemark (clear wins when
        // both are provided). NO duplicate check on update (Go parity — the DB
        // unique index still guards, surfacing as a wrapped Update failure).
        let remark = if input.clear_remark.unwrap_or(false) {
            // ClearRemark runs after SetRemark → clear wins.
            Some(None)
        } else {
            input.remark.map(Some)
        };
        let repo_input = UpdateModelInput {
            developer: input.developer,
            model_id: input.model_id,
            name: input.name,
            model_type: input
                .model_type
                .map(|t| gql_model_type_to_wire(t).to_string()),
            icon: input.icon.map(Some),
            group: input.group,
            model_card: input
                .model_card
                .as_ref()
                .map(crate::conv::model_card_input_to_json),
            settings: input
                .settings
                .as_ref()
                .map(crate::conv::model_settings_input_to_json),
            remark,
            status: input.status.map(|s| model_status_to_wire(s).to_string()),
            updated_at: now,
        };
        let row = self
            .model_repo
            .update_model(&ctx, &db_id, repo_input)
            .await
            .map_err(|e| ModelServiceError::Update(e.to_string()))?;
        Ok(crate::conv::model_row_to_gql(row))
    }

    async fn delete_model(&self, id: &str) -> Result<(), ModelServiceError> {
        // Go hard-deletes (`Model.DeleteOneID`); the repo only exposes soft
        // delete. Soft-delete hides the row from every default query — the
        // effect the admin UI observes. Documented divergence (see adapter doc).
        let ctx = boot_request_context();
        let db_id = model_db_id_from_gql_str(id)?;
        let now = chrono::Utc::now().to_rfc3339();
        self.model_repo
            .soft_delete_model(&ctx, &db_id, &now)
            .await
            .map_err(|e| ModelServiceError::Delete(e.to_string()))?;
        Ok(())
    }
}

/// System boot runs before any authenticated principal exists, so repo access
/// uses the `Test` principal — `conduit-db` policy treats `PrincipalKind::System
/// | Test` as a trusted bypass (see `policy.rs`), matching Go's pre-auth system
/// init path.
fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

#[async_trait]
impl SystemService for DbSystemService {
    async fn is_initialized(&self) -> Result<bool, String> {
        let ctx = boot_request_context();
        self.system
            .is_initialized(&ctx)
            .await
            .map_err(|err| err.to_string())
    }

    async fn initialize(&self, params: InitializeSystemParams) -> Result<(), String> {
        let domain_params = InitializeParams {
            owner_email: params.owner_email,
            owner_password: params.owner_password,
            owner_first_name: non_empty_option(params.owner_first_name),
            owner_last_name: non_empty_option(params.owner_last_name),
            brand_name: params.brand_name,
            prefer_language: non_empty_option(params.prefer_language),
            accounting_settings: params.accounting_settings,
            // Go records build.Version; the binary's package version stands in
            // until the build script stamps CONDUIT_BUILD_VERSION. Empty would
            // skip the write, but recording it is more faithful.
            version: env!("CARGO_PKG_VERSION").to_string(),
            now: chrono::Utc::now().to_rfc3339(),
        };
        crate::wiring_postgres_system_initialize::initialize_system(&self.pool, &domain_params)
            .await?;

        // The transaction bypasses the repository objects so all potentially
        // cached bootstrap values must be evicted after the commit. Cache
        // invalidation is best-effort: returning an error after a successful
        // commit would make a safe retry look like a failed initialization.
        for key in [
            system_key::JWT_SECRET_KEY,
            system_key::BRAND_NAME,
            system_key::DEFAULT_DATA_STORAGE_ID,
            system_key::GENERAL_SETTINGS,
            system_key::VERSION,
            system_key::INITIALIZED,
        ] {
            if let Err(error) = self.system.invalidate_system_value_cache(key).await {
                tracing::warn!(key, %error, "failed to invalidate committed bootstrap setting");
            }
        }
        Ok(())
    }

    async fn brand_logo(&self) -> Result<String, String> {
        let ctx = boot_request_context();
        self.system
            .brand_logo(&ctx)
            .await
            .map_err(|err| err.to_string())
    }

    async fn jwt_secret(&self) -> Result<Option<Vec<u8>>, String> {
        let ctx = boot_request_context();
        // Mirrors Go `SystemService.SecretKey` (biz/system.go:783-794): read the
        // hex-encoded secret persisted at `initialize` time. When the system is
        // not initialized the domain service returns `SystemNotInitialized`,
        // which we surface as `Ok(None)` so the middleware falls through to its
        // config fallback (matching Go's ErrSystemNotInitialized -> no secret).
        let secret = match self.system.secret_key(&ctx).await {
            Ok(secret) => secret,
            Err(conduit_services::ServiceError::SystemNotInitialized) => return Ok(None),
            Err(err) => return Err(err.to_string()),
        };

        // The stored value is hex-encoded (Go `hex.EncodeToString`); decode it to
        // the raw signing bytes so it matches the bytes `DbSigninService::
        // generate_jwt_token` signs with (which also hex-decodes before signing).
        let bytes = decode_hex(secret.as_str()).map_err(|err| match err {
            SigninError::Internal(msg) => msg,
            SigninError::InvalidCredentials | SigninError::UserInactive => {
                "invalid jwt secret encoding".to_string()
            }
        })?;
        Ok(Some(bytes))
    }

    async fn blocked_ips(&self) -> Result<Vec<String>, String> {
        let ctx = boot_request_context();
        self.system
            .security_settings(&ctx)
            .await
            .map(|settings| settings.blocked_ips)
            .map_err(|err| err.to_string())
    }
}

/// Map the handler's plain `String` (Go zero-fills missing fields) to the
/// domain's `Option<String>`: an empty/whitespace value becomes `None` so the
/// service applies its Go-compatible defaults (e.g. `prefer_language` → "en").
fn non_empty_option(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Resolves a JWT user's authorization facts for the auth middleware.
///
/// This is the Rust counterpart of the user load Go performs inside
/// `AuthenticateJWTToken` (`biz/auth.go:191-201`): the row carries `is_owner`
/// and the role-expanded `scopes`, which Go's RBAC then reads via
/// `contexts.GetUser`. Without it a JWT principal reaches the policy layer with
/// an empty scope set, so every scope check denies — the reason the admin
/// adapters fell back to a bypass principal.
///
/// Reuses `AuthUserRepo::find_user_by_id` (the same lookup
/// `AuthService::authenticate_jwt` performs) and enforces the same
/// activated-user requirement.
struct DbJwtIdentityResolver {
    user_repo: Arc<dyn AuthUserRepo>,
}

#[async_trait]
impl conduit_http::middleware::JwtIdentityResolver for DbJwtIdentityResolver {
    async fn resolve(&self, user_id: i64) -> conduit_http::middleware::JwtIdentityResolution {
        use conduit_http::middleware::{JwtIdentityResolution, JwtUserIdentity};
        // Repo reads run under the system principal, mirroring Go's
        // `authz.RunWithSystemBypass(ctx, "auth-lookup", ...)`: this lookup is
        // the auth layer establishing *who* the caller is — it cannot itself
        // require a resolved principal.
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let user = match self
            .user_repo
            .find_user_by_id(&ctx, &user_id.to_string())
            .await
        {
            // Go: `failed to get user` and a missing row both wrap
            // `ErrInvalidJWT` -> 401. Deleted users are filtered by the repo's
            // `deleted_at = 0` predicate, so they land here too (P-33).
            Ok(None) | Err(_) => return JwtIdentityResolution::UserUnavailable,
            Ok(Some(user)) => user,
        };
        // Go: `user not activated` wraps `ErrInvalidJWT` -> 401.
        if user.status != AuthUserStatus::Active {
            return JwtIdentityResolution::UserUnavailable;
        }
        JwtIdentityResolution::Found(JwtUserIdentity {
            is_owner: user.is_owner,
            scope_slugs: user.scope_slugs,
        })
    }
}

/// Handler-facing [`SigninService`] backed by the domain `AuthService` over
/// PostgreSQL. The host owns this bridge so `conduit-http` stays decoupled
/// from `conduit-services`/`conduit-db`.
struct DbSigninService {
    /// Domain auth service for credential validation.
    auth: Arc<AuthService>,
    /// System repo for fetching the JWT secret at token-generation time.
    /// The secret is persisted by `initialize` under `system_jwt_secret_key`.
    system_repo: Arc<dyn conduit_db::repo::SystemRepo>,
    session_ttl: std::time::Duration,
}

/// Simple hex decoder (lowercase hex, matching Go's `hex.EncodeToString` output).
/// Mirrors the private `decode_hex` in `conduit-auth/src/password.rs`.
fn decode_hex(value: &str) -> Result<Vec<u8>, SigninError> {
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(SigninError::Internal(
            "hex string has odd length".to_string(),
        ));
    }

    let mut out = Vec::with_capacity(bytes.len() / 2);
    for (pair_index, pair) in bytes.chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0], pair_index * 2)?;
        let low = decode_nibble(pair[1], pair_index * 2 + 1)?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

/// Decode a single hex nibble (0-9, a-f).
fn decode_nibble(byte: u8, index: usize) -> Result<u8, SigninError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(SigninError::Internal(format!(
            "invalid hex byte at index {index}: 0x{byte:02x}"
        ))),
    }
}

impl DbSigninService {
    fn new(
        auth: Arc<AuthService>,
        system_repo: Arc<dyn conduit_db::repo::SystemRepo>,
        session_ttl: std::time::Duration,
    ) -> Self {
        Self {
            auth,
            system_repo,
            session_ttl,
        }
    }
}

#[async_trait]
impl SigninService for DbSigninService {
    async fn authenticate_user(
        &self,
        email: &str,
        password: &str,
    ) -> Result<HandlerAuthenticatedUser, SigninError> {
        let ctx = boot_request_context();
        let domain_user = self
            .auth
            .authenticate_user(&ctx, email, password)
            .await
            .map_err(|e| match e {
                AuthServiceError::InvalidCredentials => SigninError::InvalidCredentials,
                AuthServiceError::UserInactive(_) => SigninError::UserInactive,
                _ => SigninError::Internal(e.to_string()),
            })?;

        // Convert domain `AuthenticatedUser` → handler `AuthenticatedUser`.
        // The handler version carries `i64` id (for JWT subject) and `UserInfo`
        // (the frontend contract). Domain id is a string; parse it safely.
        let user_id_i64: i64 = domain_user
            .id
            .parse()
            .map_err(|e| SigninError::Internal(format!("invalid user id format: {e}")))?;

        // Split `display_name` into first/last names (simple space split).
        // The domain stores the combined display name; the handler expects separate fields.
        let mut parts = domain_user.display_name.splitn(2, ' ');
        let first_name = parts.next().unwrap_or("").to_string();
        let last_name = parts.next().unwrap_or("").to_string();

        // Build `UserInfo` from domain user fields. The signin flow only needs the
        // basic fields; extended fields (projects, roles, oidc_identities) are empty
        // for this minimal implementation (matching the handler's test fakery).
        let info = UserInfo {
            id: domain_user.id.clone(),
            email: domain_user.email,
            first_name,
            last_name,
            is_owner: domain_user.is_owner,
            prefer_language: "en".to_string(), // Default; can be loaded from user row if needed
            avatar: None,
            scopes: domain_user.scope_slugs,
            roles: vec![], // Minimal; can be expanded with role repo queries
            projects: vec![],
            oidc_identities: vec![],
            has_password: true, // Password auth succeeded, so user has a password
        };

        Ok(HandlerAuthenticatedUser {
            id: user_id_i64,
            info,
        })
    }

    async fn generate_jwt_token(
        &self,
        user: &HandlerAuthenticatedUser,
    ) -> Result<String, SigninError> {
        let ctx = boot_request_context();

        // Fetch the JWT secret from the system table. It was written during
        // `initialize` under the `system_jwt_secret_key` key.
        let secret_row = self
            .system_repo
            .get_system_value_unchecked(&ctx, system_key::JWT_SECRET_KEY)
            .await
            .map_err(|e| SigninError::Internal(e.to_string()))?;

        let secret_hex = secret_row.map(|r| r.value).ok_or_else(|| {
            SigninError::Internal("JWT secret not found in system table".to_string())
        })?;

        // Decode the hex secret (Go stores it as hex-encoded bytes).
        let secret_bytes = decode_hex(&secret_hex)?;

        // Build JWT claims with the user's numeric id (Go's `user_id` claim is i64).
        // The session scope is a minimal default; a full implementation would
        // include project-specific scopes.
        let ttl = chrono::Duration::from_std(self.session_ttl).map_err(|_| {
            SigninError::Internal("configured JWT session TTL is too large".to_string())
        })?;
        let claims = Claims::with_ttl(user.id, "session:project:default", ttl);

        encode_hs256(&claims, secret_bytes).map_err(|e| SigninError::Internal(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Model domain wiring — bridges handler-facing `HttpModelService` to the
// domain `DomainModelService` (RUST-P11-001 MAP-01).
// ---------------------------------------------------------------------------

/// Handler-facing [`HttpModelService`] backed by the model repository.
struct DbModelService {
    repo: Arc<dyn ModelRepo>,
    pool: PgPool,
}

impl DbModelService {
    fn new_with_repo(repo: Arc<dyn ModelRepo>, pool: PgPool) -> Self {
        Self { repo, pool }
    }

    async fn effective_retail_prices(
        &self,
        project_id: i64,
    ) -> Result<std::collections::HashMap<String, OpenAiPricing>, ConduitError> {
        use conduit_core::objects::money::STATION_CREDIT_CODE;
        use rust_decimal::prelude::ToPrimitive;

        let accounting = crate::usage_charge_settler_postgres::load_accounting_settings(&self.pool)
            .await
            .map_err(ConduitError::internal)?;
        let multiplier =
            crate::wiring_project_access::resolve_effective_project_price_multiplier_postgres(
                &self.pool,
                project_id,
                chrono::Utc::now(),
            )
            .await
            .map_err(|error| ConduitError::internal(error.to_string()))?;
        let version = sqlx::query_as::<_, (i64, String)>(
            "SELECT v.id, b.currency FROM price_books b \
             JOIN price_book_versions v ON v.price_book_id = b.id \
             WHERE b.is_default = TRUE AND b.status = 'enabled' \
               AND v.status = 'published' \
               AND (v.effective_start_at IS NULL OR v.effective_start_at <= now()) \
               AND (v.effective_end_at IS NULL OR v.effective_end_at > now()) \
             ORDER BY v.version DESC, v.id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| ConduitError::internal(error.to_string()))?;
        let Some((version_id, currency)) = version else {
            return Ok(std::collections::HashMap::new());
        };
        if !currency.eq_ignore_ascii_case(&accounting.accounting_currency) {
            return Err(ConduitError::internal(format!(
                "published retail price currency {currency} does not match accounting currency {}",
                accounting.accounting_currency
            )));
        }

        let rows = sqlx::query_as::<_, (String, sqlx::types::Json<core_pricing::ModelPrice>)>(
            "SELECT m.model_id, i.price FROM price_book_items i \
             JOIN models m ON m.id = i.public_model_id \
             WHERE i.price_book_version_id = $1 AND m.status = 'enabled' AND m.deleted_at = 0",
        )
        .bind(version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| ConduitError::internal(error.to_string()))?;

        let scale = multiplier * accounting.credits_per_accounting_unit;
        let mut prices = std::collections::HashMap::with_capacity(rows.len());
        for (model_id, sqlx::types::Json(price)) in rows {
            let mut input = None;
            let mut output = None;
            let mut cache_read = None;
            let mut cache_write = None;
            for item in price.items {
                let amount = item
                    .pricing
                    .usage_per_unit
                    .and_then(|value| (value * scale).to_f64());
                match item.item_code.as_str() {
                    core_pricing::price_item_code::USAGE => input = amount,
                    core_pricing::price_item_code::COMPLETION => output = amount,
                    core_pricing::price_item_code::PROMPT_CACHED_TOKEN => cache_read = amount,
                    core_pricing::price_item_code::WRITE_CACHED_TOKENS => cache_write = amount,
                    _ => {}
                }
            }
            if input.is_some() || output.is_some() || cache_read.is_some() || cache_write.is_some()
            {
                prices.insert(
                    model_id,
                    OpenAiPricing {
                        input,
                        output,
                        cache_read,
                        cache_write,
                        unit: "per_1m_tokens",
                        currency: STATION_CREDIT_CODE.to_string(),
                        display_name: accounting.credit_display_name.clone(),
                    },
                );
            }
        }
        Ok(prices)
    }
}

#[async_trait]
impl HttpModelService for DbModelService {
    async fn list_enabled_models(&self) -> Result<Vec<ModelRow>, ConduitError> {
        let ctx = boot_request_context();
        // `ModelRepo` has no dedicated "enabled" query; page through
        // `list_models` and filter on the Go `status = "enabled"` value
        // (models.go: `StatusEnabled Status = "enabled"`). A generous limit
        // keeps this single-shot for the model counts a gateway realistically
        // serves; pagination can be layered later if catalogs grow large.
        let query = ListModelsQuery {
            limit: 1000,
            offset: 0,
            after_created_at: None,
            after_id: None,
        };
        let result = self
            .repo
            .list_models(&ctx, &query)
            .await
            .map_err(|e| ConduitError::internal(e.to_string()))?;
        let rows = result
            .rows
            .into_iter()
            .filter(|m| m.status == "enabled")
            .map(|m| ModelRow {
                // Public model id (Go `biz.Model.ID` = the model_id column).
                id: m.model_id,
                // Configured models are owned by "configured" in Go
                // (`biz/model.go:727`); channel-derived owners aren't tracked
                // on the models table row.
                owned_by: "configured".to_string(),
                // Unix seconds, matching Go `m.CreatedAt.Unix()`.
                created: m.created_at.timestamp(),
                name: Some(m.name),
                // `remark` is the free-text description column.
                description: m.remark,
                // The db row stores icon as a plain (possibly empty) String;
                // map empty → None so the JSON omits it like Go's `omitempty`.
                icon: if m.icon.is_empty() {
                    None
                } else {
                    Some(m.icon)
                },
                ty: Some(m.model_type),
                model_card: serde_json::from_value(m.model_card).ok(),
                retail_pricing: None,
            })
            .collect();
        Ok(rows)
    }

    async fn list_enabled_models_for_project(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<ModelRow>, ConduitError> {
        let mut rows = self.list_enabled_models().await?;
        let Some(project_id) = project_id.filter(|project_id| *project_id > 0) else {
            return Ok(rows);
        };
        let mut prices = self.effective_retail_prices(project_id).await?;
        for row in &mut rows {
            row.retail_pricing = prices.remove(&row.id);
        }
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Orchestrator bridge — implements handler-facing `OpenAiOrchestratorService`
// by delegating to the orchestrator crate's `OpenAiOrchestratorBridge`.
// ---------------------------------------------------------------------------

struct BridgeOrchestratorService {
    bridge: Arc<OpenAiOrchestratorBridge>,
    system: Arc<DomainSystemService>,
    request_repo: Arc<dyn conduit_db::repo::request_repo::RequestRepo>,
    thread_repo: Arc<dyn ThreadRepo>,
    trace_repo: Arc<dyn TraceRepo>,
    cache: Arc<dyn Cache>,
    route_affinity: Option<Arc<crate::route_affinity::RouteAffinityRuntime>>,
    request_artifact_storage:
        Arc<dyn conduit_orchestrator::middlewares::persist::RequestArtifactStorage>,
}

impl BridgeOrchestratorService {
    async fn stamp_trace_thread_rows(&self, request: &mut LlmHttpRequest) {
        let Some(project_id) = metadata_string(request, "project_id") else {
            return;
        };
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let now = chrono::Utc::now().to_rfc3339();

        let thread_row_id = if let Some(thread_key) = metadata_string(request, "thread_key") {
            match self
                .thread_repo
                .get_or_create_thread_unchecked(&ctx, &project_id, &thread_key, now.clone())
                .await
            {
                Ok(thread) => {
                    request.metadata.insert(
                        "thread_id".to_string(),
                        serde_json::Value::from(thread.id.clone()),
                    );
                    Some(thread.id)
                }
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        thread_key = %thread_key,
                        error = %error,
                        "failed to get or create thread for OpenAI request"
                    );
                    None
                }
            }
        } else {
            metadata_string(request, "thread_id")
        };

        let Some(trace_key) = metadata_string(request, "trace_key")
            .or_else(|| metadata_string(request, "session_id"))
        else {
            return;
        };

        match self
            .trace_repo
            .get_or_create_trace_unchecked(&ctx, &project_id, &trace_key, thread_row_id, now)
            .await
        {
            Ok(trace) => {
                request.metadata.insert(
                    "trace_id".to_string(),
                    serde_json::Value::from(trace.id.clone()),
                );
                request
                    .metadata
                    .entry("session_id".to_string())
                    .or_insert_with(|| serde_json::Value::from(trace_key));
            }
            Err(error) => {
                tracing::warn!(
                    project_id = %project_id,
                    trace_key = %trace_key,
                    error = %error,
                    "failed to get or create trace for OpenAI request"
                );
            }
        }
    }

    async fn stamp_sticky_channel(&self, request: &mut LlmHttpRequest) {
        const POSITIVE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
        const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

        request.metadata.remove(STICKY_CHANNEL_ID_METADATA);
        let Some(trace_id) = metadata_string(request, "trace_id") else {
            return;
        };
        let cache_key = crate::route_affinity::sticky_channel_cache_key(&trace_id);

        match self.cache.get(&cache_key).await {
            Ok(Some(value)) => {
                match crate::route_affinity::decode_sticky_channel_cache(value, chrono::Utc::now())
                {
                    Ok(crate::route_affinity::StickyChannelCacheState::Fresh(Some(channel_id)))
                        if !channel_id.trim().is_empty() =>
                    {
                        request.metadata.insert(
                            STICKY_CHANNEL_ID_METADATA.to_string(),
                            serde_json::Value::from(channel_id),
                        );
                        return;
                    }
                    Ok(crate::route_affinity::StickyChannelCacheState::Fresh(None)) => return,
                    Ok(crate::route_affinity::StickyChannelCacheState::Fresh(Some(_)))
                    | Ok(crate::route_affinity::StickyChannelCacheState::Expired) => {}
                    Err(error) => {
                        tracing::warn!(
                            trace_id = %trace_id,
                            error = %error,
                            "invalid cached sticky channel value; querying request history"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    trace_id = %trace_id,
                    error = %error,
                    "sticky channel cache lookup failed; querying request history"
                );
            }
        }

        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let channel_id = match self
            .request_repo
            .find_last_successful_channel_id_by_trace(&ctx, &trace_id)
            .await
        {
            Ok(channel_id) => channel_id,
            Err(error) => {
                tracing::warn!(
                    trace_id = %trace_id,
                    error = %error,
                    "failed to resolve sticky channel from request history"
                );
                return;
            }
        };

        let ttl = if channel_id.is_some() {
            POSITIVE_TTL
        } else {
            NEGATIVE_TTL
        };
        if let Some(cache_value) = crate::route_affinity::sticky_channel_cache_value(
            channel_id.clone(),
            chrono::Utc::now(),
            ttl,
        ) {
            if let Err(error) = self.cache.set(&cache_key, cache_value, Some(ttl)).await {
                tracing::warn!(
                    trace_id = %trace_id,
                    error = %error,
                    "failed to cache sticky channel lookup"
                );
            }
        } else {
            tracing::warn!(trace_id = %trace_id, "sticky channel cache TTL is outside chrono range");
        }

        if let Some(channel_id) = channel_id {
            request.metadata.insert(
                STICKY_CHANNEL_ID_METADATA.to_string(),
                serde_json::Value::from(channel_id),
            );
        }
    }

    async fn stamp_route_affinity(&self, route: OpenAiRoute, request: &mut LlmHttpRequest) {
        for key in [
            ROUTE_AFFINITY_HINTS_METADATA,
            ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA,
            ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA,
            ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
            ROUTE_AFFINITY_API_FORMAT_METADATA,
            ROUTE_AFFINITY_KEY_CLASS_METADATA,
            ROUTE_AFFINITY_DECISION_METADATA,
        ] {
            request.metadata.remove(key);
        }

        let Some(runtime) = self.route_affinity.as_ref() else {
            return;
        };
        let Some(project_id) = metadata_string(request, "project_id") else {
            return;
        };
        let Some(scope) = explicit_affinity_scope(route, request) else {
            return;
        };

        request.metadata.insert(
            ROUTE_AFFINITY_PUBLIC_MODEL_METADATA.to_string(),
            serde_json::Value::from(scope.public_model_id.clone()),
        );
        request.metadata.insert(
            ROUTE_AFFINITY_API_FORMAT_METADATA.to_string(),
            serde_json::Value::from(scope.api_format.clone()),
        );

        let mut keys = Vec::new();
        if let Some(value) = scope.previous_response_id {
            request.metadata.insert(
                ROUTE_AFFINITY_KEY_CLASS_METADATA.to_string(),
                serde_json::Value::from(conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID),
            );
            let key_hash = crate::route_affinity::hash_explicit_affinity_value(&value);
            request.metadata.insert(
                ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA.to_string(),
                serde_json::Value::from(key_hash.clone()),
            );
            keys.push(RouteAffinityKey {
                project_id: project_id.clone(),
                key_class: conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID.to_string(),
                key_hash,
                public_model_id: scope.public_model_id.clone(),
                api_format: scope.api_format.clone(),
            });
        }
        if let Some(value) = scope.prompt_cache_key {
            request
                .metadata
                .entry(ROUTE_AFFINITY_KEY_CLASS_METADATA.to_string())
                .or_insert_with(|| serde_json::Value::from(conduit_db::KEY_CLASS_PROMPT_CACHE_KEY));
            let key_hash = crate::route_affinity::hash_explicit_affinity_value(&value);
            request.metadata.insert(
                ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA.to_string(),
                serde_json::Value::from(key_hash.clone()),
            );
            keys.push(RouteAffinityKey {
                project_id: project_id.clone(),
                key_class: conduit_db::KEY_CLASS_PROMPT_CACHE_KEY.to_string(),
                key_hash,
                public_model_id: scope.public_model_id.clone(),
                api_format: scope.api_format.clone(),
            });
        }

        // Responses without an inbound key still need the sanitized route
        // scope below so a successful provider response id can seed the next
        // turn. With no inbound key there is nothing to look up or explain.
        if keys.is_empty() {
            return;
        }

        let mut hints = Vec::new();
        for key in keys {
            match runtime.lookup(&key).await {
                Ok(Some(row)) => hints.push(RouteAffinityHint {
                    key_class: row.key_class,
                    channel_id: row.channel_id,
                    upstream_model_id: row.upstream_model_id,
                    upstream_api_format: row.upstream_api_format,
                    credential_identity: row.credential_identity,
                }),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        key_class = %key.key_class,
                        error = %error,
                        "failed to resolve explicit route affinity"
                    );
                }
            }
        }
        if !hints.is_empty()
            && let Ok(value) = serde_json::to_value(hints)
        {
            request
                .metadata
                .insert(ROUTE_AFFINITY_HINTS_METADATA.to_string(), value);
        }
        request.metadata.insert(
            ROUTE_AFFINITY_DECISION_METADATA.to_string(),
            serde_json::Value::from(
                if request.metadata.contains_key(ROUTE_AFFINITY_HINTS_METADATA) {
                    "history_found"
                } else {
                    "no_history"
                },
            ),
        );
    }
}

struct ExplicitAffinityScope {
    public_model_id: String,
    api_format: String,
    previous_response_id: Option<String>,
    prompt_cache_key: Option<String>,
}

fn explicit_affinity_scope(
    route: OpenAiRoute,
    request: &LlmHttpRequest,
) -> Option<ExplicitAffinityScope> {
    let (api_format, accepts_previous_response_id) = match route {
        OpenAiRoute::ChatCompletions => (
            conduit_llm::ApiFormat::OpenAiChatCompletions.as_str(),
            false,
        ),
        OpenAiRoute::Responses => (conduit_llm::ApiFormat::OpenAiResponses.as_str(), true),
        OpenAiRoute::ResponsesCompact => (
            conduit_llm::ApiFormat::OpenAiResponsesCompact.as_str(),
            true,
        ),
        _ => return None,
    };
    let body = request.json_body.clone().or_else(|| {
        request
            .body
            .as_deref()
            .and_then(|body| serde_json::from_slice(body).ok())
    })?;
    let object = body.as_object()?;
    let nonempty_string = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let public_model_id = nonempty_string("model")?;
    let previous_response_id = accepts_previous_response_id
        .then(|| nonempty_string("previous_response_id"))
        .flatten();
    let prompt_cache_key = nonempty_string("prompt_cache_key");
    if previous_response_id.is_none() && prompt_cache_key.is_none() && !accepts_previous_response_id
    {
        return None;
    }
    Some(ExplicitAffinityScope {
        public_model_id,
        api_format: api_format.to_string(),
        previous_response_id,
        prompt_cache_key,
    })
}

#[cfg(test)]
mod explicit_affinity_scope_tests {
    use super::*;

    #[test]
    fn responses_extracts_only_explicit_identity_fields() {
        let request = LlmHttpRequest {
            json_body: Some(serde_json::json!({
                "model": "gpt-public",
                "input": "identical text is not an identity",
                "previous_response_id": "resp_previous",
                "prompt_cache_key": "cache-explicit"
            })),
            ..LlmHttpRequest::default()
        };

        let scope = explicit_affinity_scope(OpenAiRoute::Responses, &request)
            .expect("explicit affinity scope");

        assert_eq!(scope.public_model_id, "gpt-public");
        assert_eq!(scope.api_format, "openai/responses");
        assert_eq!(scope.previous_response_id.as_deref(), Some("resp_previous"));
        assert_eq!(scope.prompt_cache_key.as_deref(), Some("cache-explicit"));
    }

    #[test]
    fn text_without_explicit_key_never_creates_affinity() {
        let request = LlmHttpRequest {
            json_body: Some(serde_json::json!({
                "model": "gpt-public",
                "messages": [{"role": "user", "content": "same text"}]
            })),
            ..LlmHttpRequest::default()
        };

        assert!(explicit_affinity_scope(OpenAiRoute::ChatCompletions, &request).is_none());
    }

    #[test]
    fn initial_responses_request_keeps_scope_for_returned_response_id() {
        let request = LlmHttpRequest {
            json_body: Some(serde_json::json!({
                "model": "gpt-public",
                "input": "first turn"
            })),
            ..LlmHttpRequest::default()
        };

        let scope = explicit_affinity_scope(OpenAiRoute::Responses, &request)
            .expect("responses feedback scope");

        assert_eq!(scope.public_model_id, "gpt-public");
        assert_eq!(scope.api_format, "openai/responses");
        assert!(scope.previous_response_id.is_none());
        assert!(scope.prompt_cache_key.is_none());
    }

    #[test]
    fn chat_route_does_not_treat_previous_response_id_as_supported_continuity() {
        let request = LlmHttpRequest {
            body: Some(
                serde_json::to_vec(&serde_json::json!({
                    "model": "gpt-public",
                    "previous_response_id": "resp_previous"
                }))
                .unwrap_or_default(),
            ),
            ..LlmHttpRequest::default()
        };

        assert!(explicit_affinity_scope(OpenAiRoute::ChatCompletions, &request).is_none());
    }
}

fn metadata_string(request: &LlmHttpRequest, key: &str) -> Option<String> {
    request
        .metadata
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Resolve the admin-configured upstream-error exposure policy from system
/// settings.
///
/// Mirrors Go `applyUpstreamErrorPolicy` reading
/// `systemService.RetryPolicyOrDefault(ctx).UpstreamErrorPolicy`
/// (`api/upstream_error_policy.go:43`). A missing key or an unreadable value
/// falls back to Go `defaultRetryPolicy`'s `passthrough`, which leaves provider
/// errors untouched — the safe, Go-identical default.
///
/// Free function (not a method) so the resolution is unit-testable over a bare
/// system service, without standing up the whole orchestrator bridge.
async fn resolve_upstream_error_policy(system: &DomainSystemService) -> UpstreamErrorPolicy {
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    let wire = match system.retry_policy(&ctx).await {
        Ok(Some(policy)) => serde_json::to_value(&policy)
            .and_then(serde_json::from_value::<WireRetryPolicy>)
            .unwrap_or_default(),
        // Unset key (Go `RetryPolicyOrDefault` → `defaultRetryPolicy`) or a
        // read error: fall back to the default (passthrough).
        Ok(None) | Err(_) => WireRetryPolicy::default(),
    };
    let upstream = wire.upstream_error_policy;
    // Go line 44 treats `""` and `"passthrough"` alike; `parse_policy_mode`
    // mirrors that (and defensively maps unknown values to passthrough).
    match conduit_orchestrator::errors::parse_policy_mode(upstream.mode.as_str()) {
        UpstreamErrorPolicyMode::Passthrough => UpstreamErrorPolicy::passthrough(),
        UpstreamErrorPolicyMode::Hidden => UpstreamErrorPolicy::hidden(),
        UpstreamErrorPolicyMode::Custom => UpstreamErrorPolicy::custom(upstream.custom_message),
    }
}

/// P-48: resolve the admin-configured timezone into a [`chrono::FixedOffset`]
/// for dashboard/quota time-bucketing.
///
/// Go uses `time.LoadLocation(settings.Timezone)` (`system.go:1280`); the Rust
/// dashboard SQL buckets on a fixed second-offset (`wiring_dashboard.rs`), so we
/// resolve the IANA zone to its *current* UTC offset via `chrono-tz`. An empty
/// or unparseable zone falls back to UTC (mirroring Go's `LoadLocation` error
/// branch, `system.go:1286`). Note: a fixed offset does not track DST across a
/// query window, matching how the dashboard adapter is structured (it takes a
/// single `FixedOffset`); the difference is immaterial for day/hour bucketing
/// and only at DST transitions.
pub(crate) async fn resolve_timezone_offset(system: &DomainSystemService) -> chrono::FixedOffset {
    use chrono::Offset;
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    // Read the persisted general settings JSON directly (same as the
    // `general_settings` adapter): missing key → default (UTC).
    let wire = system
        .get_json::<WireGeneralSettings>(&ctx, system_key::GENERAL_SETTINGS)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let tz_name = wire.timezone;
    if tz_name.trim().is_empty() {
        return chrono::Utc.fix();
    }
    match tz_name.parse::<chrono_tz::Tz>() {
        Ok(tz) => chrono::Utc::now().with_timezone(&tz).offset().fix(),
        Err(_) => {
            tracing::warn!(timezone = %tz_name, "unknown timezone; falling back to UTC");
            chrono::Utc.fix()
        }
    }
}

struct SystemRuntimeRetryPolicySource {
    system: Arc<DomainSystemService>,
}

impl SystemRuntimeRetryPolicySource {
    fn new(system: Arc<DomainSystemService>) -> Self {
        Self { system }
    }
}

#[async_trait]
impl RuntimeRetryPolicySource for SystemRuntimeRetryPolicySource {
    async fn current(&self) -> RuntimeRetryPolicy {
        resolve_runtime_retry_policy(self.system.as_ref()).await
    }
}

/// Read and normalize one policy snapshot for both load balancing and the
/// pipeline. Response timeouts are stored in seconds and consumed in
/// milliseconds; values are clamped to the Go-compatible `[0, 600s]` range.
async fn resolve_runtime_retry_policy(system: &DomainSystemService) -> RuntimeRetryPolicy {
    runtime_retry_policy_from_wire(resolve_wire_retry_policy(system).await)
}

fn runtime_retry_policy_from_wire(wire: WireRetryPolicy) -> RuntimeRetryPolicy {
    let clamp_secs = |s: u64| s.min(600);
    let stream_first_event_timeout_ms = clamp_secs(wire.stream_first_event_timeout_seconds) * 1000;
    let non_stream_timeout_ms = clamp_secs(wire.non_stream_response_timeout_seconds) * 1000;

    RuntimeRetryPolicy {
        load_balancer: LbRetryPolicy {
            enabled: wire.enabled,
            max_channel_retries: wire.max_channel_retries.max(0) as u32,
            max_single_channel_retries: wire.max_single_channel_retries.max(0) as u32,
            retry_delay_ms: wire.retry_delay_ms.max(0) as u64,
            strategy: conduit_orchestrator::load_balancer::LoadBalancerStrategy::parse(
                &wire.load_balancer_strategy,
            ),
        },
        pipeline: conduit_pipeline::pipeline::RetryPolicy {
            enabled: wire.enabled,
            max_channel_retries: wire.max_channel_retries.max(0) as u32,
            max_single_channel_retries: wire.max_single_channel_retries.max(0) as u32,
            retry_delay_ms: wire.retry_delay_ms.max(0) as u64,
            stream_first_event_timeout_ms,
            non_stream_timeout_ms,
            empty_response_detection: wire.empty_response_detection,
        },
        cost_score_weight: i64::from(wire.cost_score_weight.clamp(0, 100)),
    }
}

async fn resolve_wire_retry_policy(system: &DomainSystemService) -> WireRetryPolicy {
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    match system.retry_policy(&ctx).await {
        Ok(Some(policy)) => match serde_json::to_value(&policy)
            .and_then(serde_json::from_value::<WireRetryPolicy>)
        {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, "invalid persisted retry policy; using defaults");
                WireRetryPolicy::default()
            }
        },
        Ok(None) => WireRetryPolicy::default(),
        Err(error) => {
            tracing::warn!(%error, "failed to load retry policy; using defaults");
            WireRetryPolicy::default()
        }
    }
}

#[cfg(test)]
mod runtime_retry_policy_tests {
    use super::*;

    #[test]
    fn one_wire_snapshot_drives_routing_and_pipeline_consistently() {
        let runtime = runtime_retry_policy_from_wire(WireRetryPolicy {
            enabled: false,
            max_channel_retries: -1,
            max_single_channel_retries: 4,
            retry_delay_ms: 250,
            stream_first_event_timeout_seconds: 700,
            non_stream_response_timeout_seconds: 9,
            load_balancer_strategy: "failover".to_string(),
            cost_score_weight: 125,
            empty_response_detection: true,
            ..WireRetryPolicy::default()
        });

        assert!(!runtime.load_balancer.enabled);
        assert!(!runtime.pipeline.enabled);
        assert_eq!(runtime.load_balancer.max_channel_retries, 0);
        assert_eq!(runtime.pipeline.max_channel_retries, 0);
        assert_eq!(runtime.load_balancer.max_single_channel_retries, 4);
        assert_eq!(runtime.pipeline.max_single_channel_retries, 4);
        assert_eq!(runtime.load_balancer.retry_delay_ms, 250);
        assert_eq!(runtime.pipeline.retry_delay_ms, 250);
        assert_eq!(runtime.pipeline.stream_first_event_timeout_ms, 600_000);
        assert_eq!(runtime.pipeline.non_stream_timeout_ms, 9_000);
        assert!(runtime.pipeline.empty_response_detection);
        assert_eq!(runtime.cost_score_weight, 100);
        assert_eq!(
            runtime.load_balancer.strategy,
            conduit_orchestrator::load_balancer::LoadBalancerStrategy::Failover
        );
    }

    #[test]
    fn auto_disable_rules_are_validated_before_storage() {
        let valid = WireAutoDisableChannel {
            enabled: true,
            statuses: vec![WireAutoDisableChannelStatus {
                status: 401,
                times: 3,
            }],
        };
        assert!(validate_wire_auto_disable_channel(&valid).is_ok());

        for statuses in [
            vec![WireAutoDisableChannelStatus {
                status: 200,
                times: 1,
            }],
            vec![WireAutoDisableChannelStatus {
                status: 500,
                times: 0,
            }],
            vec![
                WireAutoDisableChannelStatus {
                    status: 500,
                    times: 1,
                },
                WireAutoDisableChannelStatus {
                    status: 500,
                    times: 2,
                },
            ],
        ] {
            assert!(
                validate_wire_auto_disable_channel(&WireAutoDisableChannel {
                    enabled: true,
                    statuses,
                })
                .is_err()
            );
        }
    }
}

#[async_trait]
impl OpenAiOrchestratorService for BridgeOrchestratorService {
    async fn process(
        &self,
        route: OpenAiRoute,
        mut request: LlmHttpRequest,
    ) -> Result<OpenAiHandlerOutput, ConduitError> {
        // Stamp live_preview_enabled from system storage policy so the
        // LivePreviewMiddleware can read it from PipelineContext.metadata.
        // Mirrors Go `livePreviewMiddleware.OnInboundLlmRequest` which reads
        // `systemService.StoragePolicyOrDefault(ctx).LivePreview`.
        let sys_ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let (pass_through, user_agent_pass_through) = tokio::join!(
            self.system.pass_through(&sys_ctx),
            self.system.user_agent_pass_through(&sys_ctx)
        );
        let pass_through = match pass_through {
            Ok(enabled) => enabled,
            Err(error) => {
                tracing::warn!(%error, "failed to load global pass-through setting");
                false
            }
        };
        let user_agent_pass_through = match user_agent_pass_through {
            Ok(enabled) => enabled,
            Err(error) => {
                tracing::warn!(%error, "failed to load global User-Agent pass-through setting");
                false
            }
        };
        stamp_global_pass_through_metadata(&mut request, pass_through, user_agent_pass_through);
        let policy = self.system.storage_policy_or_default(&sys_ctx).await;
        if policy.live_preview {
            request.metadata.insert(
                "live_preview_enabled".to_string(),
                serde_json::Value::from("true"),
            );
        }
        request.metadata.insert(
            "storage_store_request_headers".to_string(),
            serde_json::Value::from(policy.store_request_headers.to_string()),
        );
        request.metadata.insert(
            "storage_store_request_body".to_string(),
            serde_json::Value::from(policy.store_request_body.to_string()),
        );
        request.metadata.insert(
            "storage_store_response_body".to_string(),
            serde_json::Value::from(policy.store_response_body.to_string()),
        );
        request.metadata.insert(
            "storage_store_chunks".to_string(),
            serde_json::Value::from(policy.store_chunks.to_string()),
        );
        // The storage selection is trusted system state. Never accept an
        // inherited/client-provided storage id or external-route marker.
        request.metadata.remove("data_storage_id");
        request.metadata.remove("data_storage_external");
        match self.request_artifact_storage.current_default().await {
            Ok(Some(target)) => {
                request.metadata.insert(
                    "data_storage_id".to_string(),
                    serde_json::Value::from(target.id),
                );
                request.metadata.insert(
                    "data_storage_external".to_string(),
                    serde_json::Value::from(target.external.to_string()),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to resolve default request data storage; using PostgreSQL")
            }
        }
        self.stamp_trace_thread_rows(&mut request).await;
        self.stamp_route_affinity(route, &mut request).await;
        self.stamp_sticky_channel(&mut request).await;
        let bridge_route = match route {
            OpenAiRoute::ChatCompletions => BridgeRoute::ChatCompletions,
            OpenAiRoute::Responses => BridgeRoute::Responses,
            OpenAiRoute::Embeddings => BridgeRoute::Embeddings,
            OpenAiRoute::AudioSpeech => BridgeRoute::AudioSpeech,
            OpenAiRoute::Videos => BridgeRoute::Videos,
            OpenAiRoute::ImageGenerations => BridgeRoute::ImageGenerations,
            // Go routes these three as thin wrappers over the ChatCompletion
            // dispatch (openai.go:362-378): image edits and audio
            // transcriptions/translations run their multipart bodies through
            // the same `ChatCompletion` flow, with the route-specific inbound
            // transformer selected downstream. The bridge has no dedicated
            // variant, so they map to ChatCompletions to match Go's dispatch.
            OpenAiRoute::ImageEdits
            | OpenAiRoute::AudioTranscriptions
            | OpenAiRoute::AudioTranslations => BridgeRoute::ChatCompletions,
            OpenAiRoute::AnthropicMessages => BridgeRoute::AnthropicMessages,
            OpenAiRoute::AnthropicCountTokens => BridgeRoute::AnthropicCountTokens,
            OpenAiRoute::GeminiGenerateContent => BridgeRoute::GeminiGenerateContent,
            OpenAiRoute::Completions => BridgeRoute::Completions,
            OpenAiRoute::ResponsesCompact => BridgeRoute::ResponsesCompact,
            OpenAiRoute::JinaRerank => BridgeRoute::JinaRerank,
            OpenAiRoute::JinaEmbeddings => BridgeRoute::JinaEmbeddings,
            OpenAiRoute::DoubaoCreateTask => BridgeRoute::DoubaoCreateTask,
        };

        // Go `transformOrchestratorError` (`api/upstream_error_policy.go:19-30`)
        // applies the admin-configured upstream-error policy to EVERY error
        // leaving the orchestrator, before the inbound transformer renders it.
        // Mirror that here: this is the single production error funnel for the
        // whole LLM proxy path, so masking applied here covers all 9 inbound
        // routes.
        let result = match self.bridge.process_command(bridge_route, request).await {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!(
                    error.kind = ?err.kind,
                    error.message = %err.message,
                    error.code = ?err.code,
                    error.http_status = err.http_status,
                    error.provider_status = ?err.provider_status,
                    error.source = ?err.source,
                    "orchestrator request failed before upstream error policy"
                );
                let policy = resolve_upstream_error_policy(&self.system).await;
                return Err(conduit_pipeline::apply_upstream_error_policy(&policy, err));
            }
        };

        // Convert orchestrator output types to http crate output types.
        use conduit_orchestrator::openai_bridge::OpenAiHandlerOutput as BridgeOutput;
        match result {
            BridgeOutput::NonStream(resp) => Ok(OpenAiHandlerOutput::NonStream(
                conduit_http::openai_handlers::OpenAiHandlerResponse {
                    status: resp.status,
                    content_type: resp.content_type,
                    body: resp.body,
                },
            )),
            BridgeOutput::Stream(events) => {
                // Go wraps the stream in `upstreamErrorStream` whose `Err()`
                // runs the same policy (`upstream_error_policy.go:125-137`), so
                // a terminal stream error is masked identically. Resolve the
                // policy once, and only when an error is actually present, to
                // keep the success path free of an extra settings read.
                let policy = if events.iter().any(|r| r.is_err()) {
                    Some(resolve_upstream_error_policy(&self.system).await)
                } else {
                    None
                };
                let mapped: Vec<Result<conduit_http::openai_handlers::StreamEvent, ConduitError>> =
                    events
                        .into_iter()
                        .map(|r| match r {
                            Ok(e) => Ok(conduit_http::openai_handlers::StreamEvent {
                                event: e.event,
                                data: e.data,
                            }),
                            Err(err) => Err(match &policy {
                                Some(policy) => {
                                    conduit_pipeline::apply_upstream_error_policy(policy, err)
                                }
                                None => err,
                            }),
                        })
                        .collect();
                Ok(OpenAiHandlerOutput::Stream(mapped))
            }
            BridgeOutput::Binary { body, content_type } => {
                Ok(OpenAiHandlerOutput::Binary { body, content_type })
            }
            // RUST-P8-003 — live incremental stream: pass the client-facing
            // event receiver straight through to the http output (the handler
            // flushes it as SSE frames as they arrive).
            BridgeOutput::LiveStream(live) => {
                // Terminal live-stream errors arrive after this method returns,
                // so apply the same final system policy in a receiver adapter.
                let policy = resolve_upstream_error_policy(&self.system).await;
                let (tx, rx) = tokio::sync::mpsc::channel(64);
                let mut upstream = live.0;
                tokio::spawn(async move {
                    while let Some(item) = upstream.recv().await {
                        let stop = item.is_err();
                        let item = item.map_err(|error| {
                            conduit_pipeline::apply_upstream_error_policy(&policy, error)
                        });
                        if tx.send(item).await.is_err() || stop {
                            break;
                        }
                    }
                });
                Ok(OpenAiHandlerOutput::LiveStream(
                    conduit_http::openai_handlers::LiveEventStream(rx),
                ))
            }
            BridgeOutput::LiveBinary {
                stream: live,
                content_type,
            } => {
                // Headers are already committed once Axum starts a binary
                // body, so a terminal provider failure can only abort that
                // body. Still apply the configured policy before forwarding
                // the typed error to avoid leaking upstream details through
                // any downstream observer.
                let policy = resolve_upstream_error_policy(&self.system).await;
                let (tx, rx) = tokio::sync::mpsc::channel(64);
                let mut upstream = live.0;
                tokio::spawn(async move {
                    while let Some(item) = upstream.recv().await {
                        let stop = item.is_err();
                        let item = item.map_err(|error| {
                            conduit_pipeline::apply_upstream_error_policy(&policy, error)
                        });
                        if tx.send(item).await.is_err() || stop {
                            break;
                        }
                    }
                });
                Ok(OpenAiHandlerOutput::LiveBinary {
                    stream: conduit_http::openai_handlers::LiveEventStream(rx),
                    content_type,
                })
            }
        }
    }
}

fn stamp_global_pass_through_metadata(
    request: &mut LlmHttpRequest,
    pass_through: bool,
    user_agent_pass_through: bool,
) {
    request.metadata.insert(
        "pass_through_enabled".to_string(),
        serde_json::Value::from(pass_through.to_string()),
    );
    request.metadata.insert(
        "pass_through_user_agent".to_string(),
        serde_json::Value::from(user_agent_pass_through.to_string()),
    );
}

#[cfg(test)]
mod pass_through_metadata_tests {
    use super::*;

    #[test]
    fn global_pass_through_settings_replace_stale_request_metadata() {
        let mut request = LlmHttpRequest::default();
        request.metadata.insert(
            "pass_through_enabled".to_string(),
            serde_json::Value::from("stale"),
        );

        stamp_global_pass_through_metadata(&mut request, true, false);

        assert_eq!(
            request
                .metadata
                .get("pass_through_enabled")
                .and_then(serde_json::Value::as_str),
            Some("true")
        );
        assert_eq!(
            request
                .metadata
                .get("pass_through_user_agent")
                .and_then(serde_json::Value::as_str),
            Some("false")
        );
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible outbound transformer — builds HTTP requests from LlmRequest
// using the channel's base_url and api_key from the candidate context.
// ---------------------------------------------------------------------------

struct OpenAiCompatOutbound;

impl conduit_transformers::OutboundTransformer for OpenAiCompatOutbound {
    fn name(&self) -> &'static str {
        "openai-compat-outbound"
    }

    fn outbound_request(
        &self,
        request: &conduit_llm::LlmRequest,
    ) -> conduit_transformers::TransformerResult<LlmHttpRequest> {
        // Build a minimal outbound HTTP request from the LlmRequest.
        // The pipeline will merge auth headers and URL from the candidate context.
        // `TransformerResult<T>` is `Result<T, ConduitError>`, so serialization
        // failures map straight to an internal `ConduitError`.
        //
        // Emit `json_body` (NOT the pre-encoded `body`): the channel
        // body/param override middleware only mutates `json_body`
        // (`override_request.rs`), and the executor prefers `body` over
        // `json_body` when both are set — so setting `body` here would make the
        // override a silent no-op (P-38). With only `json_body` set, overrides
        // apply and the executor serializes the (possibly modified) value.
        let json_body = match &request.payload {
            conduit_llm::LlmRequestPayload::Rerank(payload) => {
                let mut body = serde_json::to_value(payload).map_err(|err| {
                    ConduitError::internal("failed to serialize rerank request").with_source(err)
                })?;
                if let Some(object) = body.as_object_mut() {
                    if let Some(model) = request.model.as_ref() {
                        object.insert(
                            "model".to_string(),
                            serde_json::Value::String(model.clone()),
                        );
                    }
                    for (key, value) in &request.extra_body {
                        object.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
                body
            }
            _ => conduit_transformers::openai_outbound::build_openai_outbound_body(request)?,
        };
        Ok(LlmHttpRequest {
            method: "POST".to_string(),
            path: openai_compat_upstream_path(request.api_format).to_string(),
            api_format: Some(request.api_format),
            json_body: Some(json_body),
            ..LlmHttpRequest::default()
        })
    }

    fn outbound_response(
        &self,
        mut response: conduit_llm::HttpResponse,
    ) -> conduit_transformers::TransformerResult<conduit_llm::HttpResponse> {
        if response.usage.is_none()
            && let Some(body) = response.json_body.as_ref()
        {
            response.usage = conduit_transformers::openai_outbound::extract_usage(body);
        }
        Ok(response)
    }

    fn outbound_stream_event(
        &self,
        event: conduit_llm::StreamEvent,
    ) -> conduit_transformers::TransformerResult<conduit_llm::StreamEvent> {
        Ok(event)
    }

    fn outbound_error(
        &self,
        response: conduit_llm::HttpResponse,
    ) -> conduit_transformers::TransformerResult<ConduitError> {
        let status = response.status;
        // The executor decodes JSON responses into `json_body` and leaves
        // `body` empty. Reading only `body` therefore discarded every normal
        // OpenAI-compatible error envelope and produced an empty execution
        // error. Prefer the decoded JSON, falling back to raw bytes.
        let provider_body = response.json_body.or_else(|| {
            response.body.as_deref().map(|bytes| {
                serde_json::from_slice(bytes)
                    .unwrap_or_else(|_| serde_json::json!({"body": String::from_utf8_lossy(bytes)}))
            })
        });
        let message = provider_body
            .as_ref()
            .and_then(|body| body.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Upstream provider request failed")
            .to_string();
        let code = provider_body
            .as_ref()
            .and_then(|body| body.get("error"))
            .and_then(|error| error.get("code").or_else(|| error.get("type")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        // Go's inbound error conversion preserves the provider StatusCode.
        // Keep it separately for retry/operations accounting and use it as the
        // client status when it is a valid HTTP error status.
        let client_status = if (400..=599).contains(&status) {
            status
        } else {
            502
        };
        let mut error = ConduitError::upstream(message.clone())
            .with_provider_status(status)
            .with_http_status(client_status)
            .with_safe_message(message);
        if let Some(code) = code {
            error = error.with_code(code);
        }
        if let Some(body) = provider_body {
            error = error.with_provider_body(body);
        }
        Ok(error)
    }
}

fn openai_compat_upstream_path(api_format: conduit_llm::ApiFormat) -> &'static str {
    use conduit_llm::ApiFormat;
    match api_format {
        ApiFormat::OpenAiCompletions => "/v1/completions",
        ApiFormat::OpenAiResponses => "/v1/responses",
        ApiFormat::OpenAiResponsesCompact => "/v1/responses/compact",
        ApiFormat::OpenAiImageGeneration => "/v1/images/generations",
        ApiFormat::OpenAiImageEdit => "/v1/images/edits",
        ApiFormat::OpenAiImageVariation => "/v1/images/variations",
        ApiFormat::OpenAiEmbeddings | ApiFormat::GeminiEmbeddings => "/v1/embeddings",
        ApiFormat::OpenAiVideo | ApiFormat::SeedanceVideo => "/v1/videos",
        ApiFormat::OpenAiAudioSpeech => "/v1/audio/speech",
        ApiFormat::OpenAiAudioTranscriptions => "/v1/audio/transcriptions",
        ApiFormat::OpenAiAudioTranslations => "/v1/audio/translations",
        ApiFormat::JinaRerank => "/v1/rerank",
        ApiFormat::JinaEmbeddings => "/jina/v1/embeddings",
        ApiFormat::OpenAiChatCompletions
        | ApiFormat::GeminiContents
        | ApiFormat::AnthropicMessages
        | ApiFormat::AiSdkText
        | ApiFormat::AiSdkDatastream
        | ApiFormat::OllamaChat => "/v1/chat/completions",
    }
}

#[cfg(test)]
mod postgres_runtime_boot_tests {
    use super::*;

    #[tokio::test]
    async fn build_runtime_services_starts_postgres_core_when_dsn_is_provided() -> Result<(), String>
    {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let config = conduit_config::model::AppConfig {
            db: conduit_config::model::DatabaseConfig {
                dialect: "postgres".into(),
                dsn,
                ..Default::default()
            },
            ..Default::default()
        };
        let (services, _pools, _) = build_runtime_services(&config).await?;
        let system = services
            .system_service()
            .ok_or("postgres system service missing")?;
        // Startup must work for both a fresh database and an already
        // initialized production database.
        let _initialized = system.is_initialized().await?;
        assert!(services.signin_service().is_some());
        assert!(services.signup_service().is_some());
        assert!(services.model_service().is_some());
        assert!(services.api_key_validation_service().is_some());
        assert!(services.admin_schema().is_some());
        Ok(())
    }
}

#[cfg(test)]
mod postgres_accounting_currency_tests {
    use super::*;
    use conduit_admin_graphql::system::SystemSettingsServices;
    use rust_decimal::Decimal;

    fn settings_adapter(pool: PgPool) -> SystemSettingsAdapter {
        let system = Arc::new(DomainSystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
            Arc::new(conduit_cache::NoopCache::new()),
        ));
        SystemSettingsAdapter {
            system,
            pool,
            http: reqwest::Client::new(),
            started_at: std::time::Instant::now(),
        }
    }

    #[tokio::test]
    async fn accounting_currency_lock_reflects_every_price_table()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = &database.pool;

        assert!(!postgres_accounting_currency_locked(pool).await?);

        sqlx::query(
            "INSERT INTO price_books(name,currency,status,is_default) VALUES('lock-retail','CNY','enabled',FALSE)",
        )
        .execute(pool)
        .await?;
        assert!(postgres_accounting_currency_locked(pool).await?);
        sqlx::query("DELETE FROM price_books").execute(pool).await?;

        sqlx::query(
            "INSERT INTO price_book_versions(price_book_id,version,status,reference_id) VALUES(42,1,'published','lock-retail-version')",
        )
        .execute(pool)
        .await?;
        assert!(postgres_accounting_currency_locked(pool).await?);
        sqlx::query("DELETE FROM price_book_versions")
            .execute(pool)
            .await?;

        sqlx::query(
            "INSERT INTO channel_model_prices(channel_id,model_id,currency_code,price,reference_id) \
             VALUES(7,'lock-channel','USD','{\"items\":[]}'::jsonb,'lock-channel-head')",
        )
        .execute(pool)
        .await?;
        assert!(postgres_accounting_currency_locked(pool).await?);
        sqlx::query("DELETE FROM channel_model_prices")
            .execute(pool)
            .await?;

        sqlx::query(
            "INSERT INTO channel_model_price_versions \
             (channel_id,model_id,channel_model_price_id,currency_code,price,status,effective_start_at,reference_id) \
             VALUES(7,'lock-channel-version',77,'EUR','{\"items\":[]}'::jsonb,'archived',now(),'lock-channel-version')",
        )
        .execute(pool)
        .await?;
        assert!(postgres_accounting_currency_locked(pool).await?);

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn accounting_currency_changes_only_before_prices_exist()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let adapter = settings_adapter(database.pool.clone());

        let mut settings = adapter.general_settings().await?;
        assert!(!settings.accounting_currency_locked);
        settings.accounting_currency_code = "usd".to_string();
        adapter.set_general_settings(Some(41), settings).await?;
        let settings = adapter.general_settings().await?;
        assert_eq!(settings.accounting_currency_code, "USD");

        sqlx::query(
            "INSERT INTO price_books(name,currency,status,is_default) VALUES('currency-lock','USD','enabled',FALSE)",
        )
        .execute(&database.pool)
        .await?;
        let mut settings = adapter.general_settings().await?;
        assert!(settings.accounting_currency_locked);

        settings.accounting_currency_code = "EUR".to_string();
        let error = adapter
            .set_general_settings(None, settings)
            .await
            .expect_err("a different accounting currency must be rejected once prices exist");
        assert!(
            error
                .to_string()
                .contains("cannot be changed after any retail or channel procurement price exists"),
            "unexpected error: {error}"
        );

        let mut settings = adapter.general_settings().await?;
        settings.credit_display_name = "博丽神社奉纳".to_string();
        settings.credits_per_accounting_unit =
            conduit_admin_graphql::scalars::DecimalScalar(Decimal::from(12_345));
        settings.timezone = "Asia/Shanghai".to_string();
        adapter.set_general_settings(Some(42), settings).await?;

        let stored = adapter.general_settings().await?;
        assert_eq!(stored.accounting_currency_code, "USD");
        assert_eq!(stored.credit_display_name, "博丽神社奉纳");
        assert_eq!(stored.credits_per_accounting_unit.0, Decimal::from(12_345));
        assert_eq!(stored.timezone, "Asia/Shanghai");
        assert!(stored.accounting_currency_locked);
        let audits = sqlx::query_as::<_, (Option<i64>, String, i64)>(
            "SELECT actor_id,accounting_currency,accounting_settings_version \
             FROM pricing_change_audits WHERE entity_type='accounting_settings' ORDER BY id",
        )
        .fetch_all(&database.pool)
        .await?;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].0, Some(41));
        assert_eq!(audits[0].1, "USD");
        assert_eq!(audits[1].0, Some(42));
        assert_eq!(audits[1].1, "USD");
        assert!(audits.iter().all(|audit| audit.2 > 0));

        drop(adapter);
        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn settings_writer_waits_for_shared_accounting_lock()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let adapter = Arc::new(settings_adapter(database.pool.clone()));
        let mut settings = adapter.general_settings().await?;
        settings.accounting_currency_code = "EUR".to_string();

        let mut blocker = database.pool.begin().await?;
        lock_accounting_currency_price_writes(&mut blocker).await?;
        let writer_adapter = Arc::clone(&adapter);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut writer = tokio::spawn(async move {
            let _ = started_tx.send(());
            writer_adapter.set_general_settings(None, settings).await
        });
        started_rx.await?;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut writer)
                .await
                .is_err(),
            "settings write must wait while the shared transaction lock is held"
        );

        blocker.commit().await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await???;
        assert_eq!(
            adapter.general_settings().await?.accounting_currency_code,
            "EUR"
        );

        drop(adapter);
        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_channel_price_save_stages_draft_without_writing_formal_price()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels (\"type\",name,credentials,default_test_model) \
             VALUES ('openai','price-rollback','{}'::jsonb,'mock-chat') RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;

        let system = Arc::new(DomainSystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(database.pool.clone())),
            Arc::new(conduit_cache::NoopCache::new()),
        ));
        let adapter = ModelExtAdapter::new(
            Arc::new(conduit_db::PgModelRepo::new(database.pool.clone())),
            Arc::new(conduit_db::PgChannelRepo::new(database.pool.clone())),
            system,
            database.pool.clone(),
        );
        let formal_prices = adapter
            .save_channel_model_prices(
                Some(7),
                channel_id.to_string().into(),
                vec![
                    conduit_admin_graphql::model_ext::SaveChannelModelPriceInput {
                        model_id: "mock-chat".to_string(),
                        currency_code: "CNY".to_string(),
                        price: conduit_admin_graphql::model_ext::ModelPriceInput {
                            items: vec![conduit_admin_graphql::model_ext::ModelPriceItemInput {
                                item_code: conduit_admin_graphql::request_usage::PriceItemCode::PromptTokens,
                                pricing: conduit_admin_graphql::model_ext::PricingInput {
                                    mode: conduit_admin_graphql::model_ext::PricingMode::UsagePerUnit,
                                    flat_fee: None,
                                    usage_per_unit: Some(
                                        conduit_admin_graphql::scalars::DecimalScalar(
                                            rust_decimal::Decimal::ONE,
                                        ),
                                    ),
                                    usage_tiered: None,
                                },
                                prompt_write_cache_variants: None,
                            }],
                        },
                    },
                ],
            )
            .await?;
        assert!(formal_prices.is_empty());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1"
            )
            .bind(channel_id)
            .fetch_one(&database.pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM change_sets WHERE kind='provider_price' \
                 AND scope_type='channel' AND scope_id=$1 AND status='draft'"
            )
            .bind(channel_id.to_string())
            .fetch_one(&database.pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM change_set_items item JOIN change_sets cs \
                 ON cs.id=item.change_set_id WHERE cs.scope_id=$1 \
                 AND item.source_snapshot->>'source'='manual'"
            )
            .bind(channel_id.to_string())
            .fetch_one(&database.pool)
            .await?,
            1
        );

        database.cleanup().await?;
        Ok(())
    }
}

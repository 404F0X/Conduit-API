//! DB-backed [`CandidateSource`]: loads enabled channels from the channel repo
//! and runs the pure [`CandidateSelector`].
//!
//! This is the I/O glue between the PostgreSQL channel repository and the
//! already-ported pure selector (P9-003). It builds a
//! [`ChannelSnapshot`] per enabled channel via [`ChannelService`] (model-entry
//! map + resolved endpoints) and feeds the selector.
//!
//! **Current scope — legacy path (S05):** until model associations are loaded
//! from the DB (S06), [`LegacyAssociationSource`] resolves no Conduit API models and
//! allows the fallback-to-channels path, so `CandidateSelector::select` runs
//! `select_legacy` — matching channels by their model-entry map. This is
//! correct for the boot state (channels configured, no model entities yet) and
//! for any model that lives directly on a channel.
//!
//! **Credentials:** the snapshot carries only a `credential_key_identity`
//! placeholder here; resolving the real API key (via
//! `channel_service::credentials::decide_credential`) is the ④ outbound-wiring
//! step, not selection.

use std::sync::Arc;

use async_trait::async_trait;

use conduit_core::ConduitError;
use conduit_core::objects::ModelSettings;
use conduit_core::objects::SystemModelSettings;
use conduit_core::objects::channel_settings::{
    ChannelCredentials, ChannelEndpoint, ChannelPolicies, ChannelSettings, DisabledAPIKey,
};
use conduit_db::{ChannelRepo, ChannelRow, ModelRepo, PolicyContext, Principal, RequestContext};
use conduit_services::channel_service::credentials::{
    CredentialSnapshot, TraceKeyState, decide_credential,
};
use conduit_services::channel_service::model_sync::SupportedModelSet;
use conduit_services::channel_service::{ChannelService, DefaultEndpointRegistry};
use conduit_services::model_service::effective_model_associations;

use crate::candidates::{
    AssociationSource, CandidateRequest, ChannelModelsCandidate, ChannelSnapshot, EffectiveModel,
    ProviderQuotaStatusProvider, QuotaChannelStatusView, QuotaEnforcementSettings,
    SelectCandidatesError, SelectionInputs, select_candidates,
};
use crate::orchestrator::CandidateSource;

/// Runtime source of the provider-quota admission inputs.
///
/// Go reads both of these per request inside the `selectCandidates` middleware:
/// the enforcement switch from `systemService.QuotaEnforcementSettings(ctx)`
/// and the per-channel snapshot from `ProviderQuotaService.GetQuotaStatus`
/// (`select_candidates.go:67-68`). Both are I/O, so they stay behind this trait
/// — the orchestrator crate holds no DB or sqlx dependency.
///
/// A `DbCandidateSource` with no quota source wired behaves exactly as before:
/// [`apply_provider_quota_selector`](crate::candidates::apply_provider_quota_selector)
/// short-circuits on a `None` provider, and the default settings are disabled,
/// matching Go `defaultQuotaEnforcementSettings`.
#[async_trait]
pub trait QuotaAdmissionSource: Send + Sync {
    /// Go `systemService.QuotaEnforcementSettings(ctx)`.
    async fn enforcement_settings(&self) -> QuotaEnforcementSettings;

    /// Per-channel quota snapshots for the channels under consideration.
    /// Returning an empty map means "no quota data", which never blocks
    /// routing (Go's nil-status branch keeps the candidate).
    async fn quota_statuses(
        &self,
        channel_ids: &[String],
    ) -> std::collections::BTreeMap<String, QuotaChannelStatusView>;
}

/// Request-scoped source for system model settings that affect candidate
/// selection. The production implementation reads PostgreSQL through the
/// system service so admin changes take effect without restarting the gateway.
#[async_trait]
pub trait RoutingModelSettingsSource: Send + Sync {
    async fn current(&self) -> SystemModelSettings;
}

/// Snapshot map adapter: turns the pre-fetched statuses into the synchronous
/// [`ProviderQuotaStatusProvider`] the pure selector expects.
struct MapQuotaProvider {
    statuses: std::collections::BTreeMap<String, QuotaChannelStatusView>,
}

impl ProviderQuotaStatusProvider for MapQuotaProvider {
    fn get_quota_status(&self, channel_id: &str) -> Option<QuotaChannelStatusView> {
        self.statuses.get(channel_id).cloned()
    }
}

/// [`AssociationSource`] for the boot/legacy state: resolves no Conduit API models
/// and allows the fallback-to-channels path (Go default), forcing
/// [`CandidateSelector::select`] onto `select_legacy` (S05). Replaced by a
/// DB-backed association source once model entities are loaded (S06).
#[derive(Default)]
struct RequestAssociationSource {
    system_settings: SystemModelSettings,
    effective: Option<EffectiveModel>,
}

impl AssociationSource for RequestAssociationSource {
    fn resolve(&self, requested_model_id: &str) -> Option<EffectiveModel> {
        self.effective
            .as_ref()
            .filter(|model| model.model_id == requested_model_id)
            .cloned()
    }
    fn system_settings(&self) -> SystemModelSettings {
        self.system_settings.clone()
    }
}

/// Candidate source backed by the runtime PostgreSQL [`ChannelRepo`].
pub struct DbCandidateSource {
    channel_repo: Arc<dyn ChannelRepo>,
    model_repo: Arc<dyn ModelRepo>,
    channel_service: ChannelService,
    /// Provider-quota admission inputs. `None` disables the stage (Go's
    /// nil-provider + disabled-settings fast path).
    quota: Option<Arc<dyn QuotaAdmissionSource>>,
    /// Persisted model routing settings. `None` keeps the Go-compatible
    /// defaults for lightweight/test construction.
    model_settings: Option<Arc<dyn RoutingModelSettingsSource>>,
}

impl DbCandidateSource {
    /// Wrap a channel repo. The [`ChannelService`] uses default per-type
    /// endpoints; callers needing system-default endpoint overrides construct
    /// their own and the snapshot builder can be extended to take it.
    pub fn new(channel_repo: Arc<dyn ChannelRepo>, model_repo: Arc<dyn ModelRepo>) -> Self {
        Self {
            channel_repo,
            model_repo,
            channel_service: runtime_channel_service(),
            quota: None,
            model_settings: None,
        }
    }

    /// Wire the provider-quota admission stage (Go
    /// `WithProviderQuotaSelector`, `select_candidates.go:67-68`).
    ///
    /// Without this the stage is skipped entirely, which is why exhausted
    /// channels used to stay routable in production: the old code path called
    /// `select_with_inputs` (profile → native-tools → stream-policy only) and
    /// never reached the quota selector.
    pub fn with_quota_source(mut self, quota: Arc<dyn QuotaAdmissionSource>) -> Self {
        self.quota = Some(quota);
        self
    }

    /// Wire request-scoped system model settings into candidate selection.
    pub fn with_model_settings_source(
        mut self,
        source: Arc<dyn RoutingModelSettingsSource>,
    ) -> Self {
        self.model_settings = Some(source);
        self
    }
}

/// Production channel logic with a non-empty endpoint registry derived from
/// the provider descriptor table. Database rows commonly omit `endpoints`;
/// without these defaults their candidates would carry an empty API format.
fn runtime_channel_service() -> ChannelService {
    ChannelService::new().with_defaults(DefaultEndpointRegistry::from_provider_descriptors())
}

#[async_trait]
impl CandidateSource for DbCandidateSource {
    async fn select(
        &self,
        request: &CandidateRequest,
    ) -> Result<Vec<ChannelModelsCandidate>, ConduitError> {
        self.select_with_diagnostics(request)
            .await
            .map(|(candidates, _)| candidates)
    }

    async fn select_with_diagnostics(
        &self,
        request: &CandidateRequest,
    ) -> Result<
        (
            Vec<ChannelModelsCandidate>,
            crate::candidates::SelectionDiagnostics,
        ),
        ConduitError,
    > {
        // Selection runs before per-request auth resolves a principal, so use
        // the trusted Test principal (policy treats it as a bypass — see
        // `policy.rs`), matching the Go pre-auth candidate-selection path.
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        // These reads are independent. Running them concurrently shortens the
        // candidate-selection critical path without changing cache or
        // consistency semantics: all three still observe the same request
        // context and are materialized before pure candidate filtering starts.
        let rows_future = self.channel_repo.list_enabled_channels(&ctx);
        let model_future = self.model_repo.find_model_by_model_id(&ctx, &request.model);
        let quota_settings_future = async {
            match &self.quota {
                Some(source) => source.enforcement_settings().await,
                None => QuotaEnforcementSettings::default(),
            }
        };
        let model_settings_future = async {
            match &self.model_settings {
                Some(source) => source.current().await,
                None => SystemModelSettings::default(),
            }
        };
        let (rows_result, model_result, quota_settings, mut system_settings) = tokio::join!(
            rows_future,
            model_future,
            quota_settings_future,
            model_settings_future
        );

        let rows = rows_result.map_err(|err| {
            ConduitError::internal(format!("failed to load enabled channels: {err}"))
        })?;

        let mut snapshots: Vec<ChannelSnapshot> = rows
            .iter()
            .map(|row| build_snapshot(row, &self.channel_service))
            .collect();

        // A published channel-model offer is the canonical bridge between the
        // customer-facing model id and the concrete model accepted by one
        // channel. Inject it into the channel entry map for this request so an
        // administrator does not need to repeat the same mapping in Channel
        // settings. Only offers already resolved into the authenticated
        // Project's effective access reach this point.
        let offer_driven = apply_offer_mappings(request, &mut snapshots);

        let now = chrono::Utc::now().to_rfc3339();
        let model = model_result.map_err(|error| {
            ConduitError::internal(format!("failed to load model associations: {error}"))
        })?;
        // Offer mappings already identify the exact eligible deployments, so
        // use the channel-entry path instead of requiring a second set of
        // model-association rules. Project/key channel filters still run in
        // the normal selector stages below.
        if offer_driven {
            system_settings.fallback_to_channels_on_model_not_found = true;
        }
        let effective = if offer_driven {
            None
        } else {
            model.filter(|row| row.status == "enabled").and_then(|row| {
                let settings: ModelSettings = serde_json::from_value(row.settings).ok()?;
                Some(EffectiveModel {
                    model_id: row.model_id,
                    developer: row.developer.clone(),
                    updated_at: row.updated_at.to_rfc3339(),
                    associations: effective_model_associations(
                        &system_settings,
                        &row.developer,
                        &request.model,
                        Some(&settings),
                    ),
                    system_settings: system_settings.clone(),
                })
            })
        };
        // Provider-quota admission inputs (Go `select_candidates.go:67-68`).
        // Both are runtime I/O, so they are fetched BEFORE `SelectionInputs` is
        // built: that struct borrows a `dyn AssociationSource` (not `Sync`), so
        // holding it across an `await` would make this future non-`Send`.
        // With no quota source wired the stage is a no-op, matching Go's
        // nil-provider / disabled-settings path.
        let quota_statuses = match &self.quota {
            Some(source) => {
                // Only pay for the snapshot read when enforcement is on.
                if quota_settings.enabled {
                    let ids: Vec<String> = snapshots.iter().map(|s| s.id.clone()).collect();
                    source.quota_statuses(&ids).await
                } else {
                    std::collections::BTreeMap::new()
                }
            }
            None => std::collections::BTreeMap::new(),
        };
        let quota_provider = self.quota.as_ref().map(|_| MapQuotaProvider {
            statuses: quota_statuses,
        });

        let associations = RequestAssociationSource {
            system_settings,
            effective,
        };
        let inputs = SelectionInputs::new(request, &snapshots, &associations, &now);

        // `select_candidates` is the full Go middleware body: base selection →
        // profile/native-tools/stream-policy stages → quota admission →
        // (load balancing, applied later by the orchestrator's scoring stage) →
        // Go's empty-result semantics. The previous `select_with_inputs` call
        // skipped the quota stage entirely, so exhausted channels stayed
        // routable.
        match select_candidates(
            &inputs,
            quota_provider
                .as_ref()
                .map(|p| p as &dyn ProviderQuotaStatusProvider),
            &quota_settings,
            None,
        ) {
            Ok((candidates, diagnostics)) => Ok((candidates, diagnostics)),
            // Go returns a quota-exhausted error (surfaced to the client as
            // such) rather than a generic "no candidates" when the quota stage
            // is what emptied the list (`select_candidates.go:100-107`).
            Err(SelectCandidatesError::QuotaExhausted { model }) => Err(
                ConduitError::quota_exhausted(format!("all channels exhausted for model {model}")),
            ),
            Err(SelectCandidatesError::InvalidModel { .. }) => Err(ConduitError::not_found(
                "no candidates matched the request profile",
            )),
        }
    }
}

fn apply_offer_mappings(request: &CandidateRequest, snapshots: &mut [ChannelSnapshot]) -> bool {
    for snapshot in snapshots {
        if let Some(upstream_model) = request.project_upstream_models_by_channel.get(&snapshot.id) {
            snapshot
                .model_entries
                .insert_offer_mapping(request.model.clone(), upstream_model.clone());
        }
    }
    !request.project_upstream_models_by_channel.is_empty()
}

/// Build a [`ChannelSnapshot`] from a DB [`ChannelRow`].
///
/// `settings` → [`ChannelSettings`] (model mappings / overrides), `policies` →
/// [`ChannelPolicies`] (stream policy), `endpoints` → user endpoint overrides;
/// each is best-effort decoded (a malformed JSON blob falls back to defaults so
/// one bad channel never breaks selection for the rest). Model entries and
/// resolved endpoints are computed via [`ChannelService`], mirroring Go
/// `GetModelEntries` / `ResolveEndpoints`.
pub(crate) fn build_snapshot(row: &ChannelRow, svc: &ChannelService) -> ChannelSnapshot {
    let settings = serde_json::from_value::<ChannelSettings>(row.settings.clone()).ok();
    let effective_settings = settings.clone().unwrap_or_default();
    let policies: ChannelPolicies =
        serde_json::from_value(row.policies.clone()).unwrap_or_default();
    let endpoints: Vec<ChannelEndpoint> = row
        .endpoints
        .iter()
        .filter_map(|value| serde_json::from_value::<ChannelEndpoint>(value.clone()).ok())
        .collect();

    // The auto-synced model list is fetched from the provider, not persisted in
    // the row; pass empty here (the supported/manual lists carry the persisted
    // set).
    let set = SupportedModelSet::new(
        row.supported_models.clone(),
        row.manual_models.clone(),
        Vec::new(),
    );
    let merged = svc.supported_models(&set);
    let model_entries = svc.model_entry_map(&merged, &effective_settings);
    let resolved_endpoints = svc.resolve_endpoints(&row.channel_type, &endpoints);
    let active_credential = resolve_active_credential(row);
    // P-17: the full enabled-key set, carried so credential selection can be
    // deferred to request-execution time (when the trace id exists). Empty for
    // OAuth/Azure/GCP channels (auth materializes in the transformer layer).
    let enabled_credentials = resolve_enabled_credentials(row);

    ChannelSnapshot {
        id: row.id.clone(),
        name: row.name.clone(),
        ordering_weight: row.ordering_weight,
        tags: row.tags.clone(),
        updated_at: row.updated_at.to_rfc3339(),
        model_entries,
        resolved_endpoints,
        // TODO(④ outbound wiring): resolve via decide_credential + the channel's
        // resolved API key so two keys on one channel stay distinct candidates.
        credential_key_identity: String::new(),
        policies,
        channel_type: row.channel_type.clone(),
        base_url: row.base_url.clone(),
        active_credential,
        enabled_credentials,
        settings,
    }
}

/// Resolve the full enabled plaintext key set for a channel row (P-17).
///
/// This is the same enabled-key computation `resolve_active_credential` runs,
/// but returns *every* usable key rather than the single deterministic pick.
/// The orchestrator selects among these at request-execution time using the
/// request trace id (trace-sticky load balancing), so a channel with N keys
/// spreads load across all N instead of always hitting `enabled[0]`.
///
/// Empty for OAuth / Azure / GCP channels (no per-key selection — their auth
/// materializes in the transformer layer) and for channels with no usable key.
///
/// ⚠ The returned keys are plaintext secrets: in-memory only — never log them.
fn resolve_enabled_credentials(row: &ChannelRow) -> Vec<String> {
    let credentials: ChannelCredentials =
        serde_json::from_value(row.credentials.clone()).unwrap_or_default();
    let disabled_keys: Vec<DisabledAPIKey> =
        serde_json::from_value(row.disabled_api_keys.clone()).unwrap_or_default();
    // OAuth/Azure/GCP channels select no per-request key here (parity with
    // `resolve_active_credential` returning None for them): their auth
    // materializes in the transformer layer, so there is no per-key spread.
    if credentials.is_oauth() || credentials.azure.is_some() || credentials.gcp.is_some() {
        return Vec::new();
    }
    credentials
        .get_enabled_api_keys(&disabled_keys)
        .unwrap_or_default()
}

/// Resolve the active plaintext API key for a channel row (WIRE-06).
///
/// Mirrors the Go chain `Credentials.GetEnabledAPIKeys(DisabledAPIKeys)`
/// (`objects/channel.go`) → `getAPIKeyProvider` (`channel_llm.go`): decode the
/// row's credentials + disabled-key list, compute the enabled key set, and run
/// the ported [`decide_credential`] decision. Returns `None` for OAuth / Azure
/// / GCP channels (their auth materializes in the transformer layer) and for
/// channels without any usable key.
///
/// ⚠ The returned key is a plaintext secret: it flows in-memory only and must
/// never be logged or embedded in error text.
fn resolve_active_credential(row: &ChannelRow) -> Option<String> {
    let credentials: ChannelCredentials =
        serde_json::from_value(row.credentials.clone()).unwrap_or_default();
    let disabled_keys: Vec<DisabledAPIKey> =
        serde_json::from_value(row.disabled_api_keys.clone()).unwrap_or_default();
    let enabled_keys = credentials
        .get_enabled_api_keys(&disabled_keys)
        .unwrap_or_default();

    // No per-request trace here — selection happens before the request-scoped
    // trace exists, so the pick is the deterministic no-trace fallback.
    let snapshot = CredentialSnapshot::from_credentials(&credentials, &enabled_keys, "");
    decide_credential(&snapshot, &TraceKeyState::default())
        .api_key
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidates::{CandidateSelectionError, CandidateSelector};
    use crate::orchestrator::{CandidateProjector, DefaultCandidateProjector};
    use chrono::Utc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MutableRoutingModelSettingsSource {
        settings: Mutex<SystemModelSettings>,
        reads: AtomicUsize,
    }

    impl MutableRoutingModelSettingsSource {
        fn new(settings: SystemModelSettings) -> Self {
            Self {
                settings: Mutex::new(settings),
                reads: AtomicUsize::new(0),
            }
        }

        fn replace(&self, settings: SystemModelSettings) {
            *self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = settings;
        }
    }

    #[async_trait]
    impl RoutingModelSettingsSource for MutableRoutingModelSettingsSource {
        async fn current(&self) -> SystemModelSettings {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }
    }

    /// Minimal in-memory `ChannelRow` for a channel that directly supports
    /// `gpt-4` over the OpenAI chat-completions endpoint.
    fn row_supporting_gpt4() -> ChannelRow {
        let now = Utc::now();
        ChannelRow {
            id: "ch-1".to_string(),
            channel_type: "openai".to_string(),
            base_url: Some("https://api.openai.com".to_string()),
            website_url: None,
            quota_currency: None,
            actual_quota_used: None,
            quota_remaining: None,
            name: "OpenAI".to_string(),
            status: "enabled".to_string(),
            credentials: serde_json::json!({}),
            disabled_api_keys: serde_json::json!([]),
            supported_models: vec!["gpt-4".to_string()],
            manual_models: vec![],
            auto_sync_supported_models: false,
            auto_sync_model_pattern: String::new(),
            tags: vec![],
            default_test_model: String::new(),
            policies: serde_json::json!({}),
            settings: serde_json::json!({}),
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: vec![],
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn enabled_gpt4_model() -> conduit_db::ModelRow {
        let now = Utc::now();
        conduit_db::ModelRow {
            id: "model-1".to_string(),
            name: "GPT-4".to_string(),
            status: "enabled".to_string(),
            developer: "openai".to_string(),
            model_id: "gpt-4".to_string(),
            model_type: "chat".to_string(),
            icon: String::new(),
            group_name: String::new(),
            model_card: serde_json::json!({}),
            settings: serde_json::json!({}),
            remark: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn developer_settings_for_channel(channel_id: i64) -> SystemModelSettings {
        SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "openai".to_string(),
                associations: vec![conduit_core::objects::ModelAssociation {
                    kind: "channel_model".to_string(),
                    channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                        channel_id,
                        model_id: String::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn request_scoped_settings_can_disable_legacy_channel_fallback() {
        use conduit_llm::RequestType;

        let channel_repo = Arc::new(conduit_db::InMemoryChannelRepo::from_rows([
            row_supporting_gpt4(),
        ]));
        let model_repo = Arc::new(conduit_db::InMemoryModelRepo::new());
        let settings = Arc::new(MutableRoutingModelSettingsSource::new(
            SystemModelSettings {
                fallback_to_channels_on_model_not_found: false,
                ..Default::default()
            },
        ));
        let source = DbCandidateSource::new(channel_repo, model_repo)
            .with_model_settings_source(settings.clone());
        let request = CandidateRequest::new("gpt-4", RequestType::Chat, "openai/chat_completions");

        let result = source.select(&request).await;

        assert_eq!(
            result.as_ref().err().map(|error| error.http_status),
            Some(404)
        );
        assert_eq!(settings.reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn developer_association_changes_apply_on_the_next_request() -> Result<(), ConduitError> {
        use conduit_llm::RequestType;

        let mut first = row_supporting_gpt4();
        first.id = "1".to_string();
        first.name = "first".to_string();
        let mut second = row_supporting_gpt4();
        second.id = "2".to_string();
        second.name = "second".to_string();
        let channel_repo = Arc::new(conduit_db::InMemoryChannelRepo::from_rows([first, second]));
        let model_repo = Arc::new(conduit_db::InMemoryModelRepo::from_rows([
            enabled_gpt4_model(),
        ]));
        let settings = Arc::new(MutableRoutingModelSettingsSource::new(
            developer_settings_for_channel(1),
        ));
        let source = DbCandidateSource::new(channel_repo, model_repo)
            .with_model_settings_source(settings.clone());
        let request = CandidateRequest::new("gpt-4", RequestType::Chat, "openai/chat_completions");

        let first_candidates = source.select(&request).await?;
        assert_eq!(first_candidates.len(), 1);
        assert_eq!(first_candidates[0].channel_id, "1");

        settings.replace(developer_settings_for_channel(2));
        let second_candidates = source.select(&request).await?;
        assert_eq!(second_candidates.len(), 1);
        assert_eq!(second_candidates[0].channel_id, "2");
        assert_eq!(settings.reads.load(Ordering::SeqCst), 2);
        Ok(())
    }

    /// `build_snapshot` turns a channel row's supported-model list into a
    /// model-entry map, so the legacy selector can match the requested model.
    #[test]
    fn build_snapshot_populates_model_entries_from_supported_models() {
        let svc = runtime_channel_service();
        let row = row_supporting_gpt4();

        let snapshot = build_snapshot(&row, &svc);

        assert_eq!(snapshot.id, "ch-1");
        assert_eq!(snapshot.channel_type, "openai");
        assert!(
            snapshot.model_entries.get("gpt-4").is_some(),
            "gpt-4 must be present in the model-entry map"
        );
        assert_eq!(snapshot.resolved_endpoints.len(), 1);
        assert_eq!(
            snapshot.resolved_endpoints[0],
            ChannelEndpoint {
                api_format: "openai/chat_completions".to_string(),
                path: String::new(),
                base_url: String::new(),
                transport: "http".to_string(),
            }
        );
    }

    #[test]
    fn production_channel_service_registers_provider_defaults() {
        let svc = runtime_channel_service();

        let endpoints = svc.defaults().get("openai");
        assert_eq!(endpoints.map(|items| items.len()), Some(1));
        assert_eq!(
            endpoints
                .and_then(|items| items.first())
                .map(|endpoint| endpoint.api_format.as_str()),
            Some("openai/chat_completions")
        );
        assert!(
            endpoints
                .and_then(|items| items.first())
                .is_some_and(|endpoint| endpoint.path.is_empty())
        );
    }

    #[test]
    fn build_snapshot_preserves_explicit_endpoint_overrides() {
        let svc = runtime_channel_service();
        let mut row = row_supporting_gpt4();
        row.endpoints = vec![serde_json::json!({
            "api_format": "openai/responses",
            "path": "/custom/responses",
            "base_url": "https://responses.example",
            "transport": "websocket"
        })];

        let snapshot = build_snapshot(&row, &svc);

        assert_eq!(snapshot.resolved_endpoints.len(), 2);
        let endpoint = snapshot
            .resolved_endpoints
            .iter()
            .find(|endpoint| endpoint.api_format == "openai/responses");
        assert_eq!(
            endpoint,
            Some(&ChannelEndpoint {
                api_format: "openai/responses".to_string(),
                path: "/custom/responses".to_string(),
                base_url: "https://responses.example".to_string(),
                transport: "websocket".to_string(),
            })
        );
    }

    #[test]
    fn database_channel_ordering_weight_reaches_load_balancer_projection()
    -> Result<(), CandidateSelectionError> {
        use conduit_llm::RequestType;

        let svc = ChannelService::new();
        let mut row = row_supporting_gpt4();
        row.ordering_weight = 73;
        let snapshot = build_snapshot(&row, &svc);
        assert_eq!(snapshot.ordering_weight, 73);

        let request = CandidateRequest::new("gpt-4", RequestType::Chat, "openai/chat_completions");
        let candidates = CandidateSelector.select(
            &request,
            &[snapshot],
            &RequestAssociationSource::default(),
            &Utc::now().to_rfc3339(),
        )?;

        // Association priority and channel load-balancing weight are separate
        // dimensions. The legacy path has priority 0, while the DB channel's
        // ordering weight must survive into the LB-facing candidate.
        assert_eq!(candidates[0].priority, 0);
        assert_eq!(candidates[0].ordering_weight, 73);
        let projected = DefaultCandidateProjector.project(&candidates);
        assert_eq!(projected[0].ordering_weight, 73);
        Ok(())
    }

    #[test]
    fn build_snapshot_preserves_channel_settings() {
        let svc = ChannelService::new();
        let mut row = row_supporting_gpt4();
        row.settings = serde_json::json!({
            "passThroughBody": true,
            "rateLimit": { "rpm": 120, "maxConcurrent": 4 },
            "bodyOverrideOperations": [
                { "op": "set", "path": "temperature", "value": "0.2" }
            ]
        });

        let snapshot = build_snapshot(&row, &svc);
        let settings = match snapshot.settings.as_ref() {
            Some(s) => s,
            None => panic!("Test failure: settings must be retained"),
        };

        assert_eq!(settings.pass_through_body, Some(true));
        assert_eq!(settings.rate_limit.as_ref().and_then(|v| v.rpm), Some(120));
        assert_eq!(settings.body_override_operations.len(), 1);
    }

    /// The legacy path: with no associations and a requested model the channel
    /// supports, `CandidateSelector::select` returns one candidate for that
    /// channel.
    #[test]
    fn legacy_select_returns_candidate_for_supported_model() {
        use conduit_llm::RequestType;

        let svc = ChannelService::new();
        let snapshot = build_snapshot(&row_supporting_gpt4(), &svc);
        let channels = vec![snapshot];

        let request = CandidateRequest::new("gpt-4", RequestType::Chat, "openai/chat_completions");
        let now = Utc::now().to_rfc3339();
        let associations = RequestAssociationSource::default();

        let candidates = match CandidateSelector.select(&request, &channels, &associations, &now) {
            Ok(candidates) => candidates,
            Err(err) => panic!("supported model must yield a candidate: {err:?}"),
        };

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].channel_id, "ch-1");
    }

    #[test]
    fn offer_mapping_routes_canonical_model_to_concrete_upstream_model()
    -> Result<(), CandidateSelectionError> {
        use conduit_llm::RequestType;

        let svc = ChannelService::new();
        let mut channels = vec![build_snapshot(&row_supporting_gpt4(), &svc)];
        let mut request = CandidateRequest::new(
            "customer-model",
            RequestType::Chat,
            "openai/chat_completions",
        );
        request
            .project_upstream_models_by_channel
            .insert("ch-1".into(), "gpt-4".into());

        assert!(apply_offer_mappings(&request, &mut channels));
        let mut associations = RequestAssociationSource::default();
        associations
            .system_settings
            .fallback_to_channels_on_model_not_found = true;
        let candidates = CandidateSelector.select(
            &request,
            &channels,
            &associations,
            &Utc::now().to_rfc3339(),
        )?;

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].channel_id, "ch-1");
        assert_eq!(candidates[0].models[0].request_model, "customer-model");
        assert_eq!(candidates[0].models[0].actual_model, "gpt-4");
        Ok(())
    }

    /// Single legacy `credentials.apiKey` (Go `objects/channel.go`
    /// `GetAllAPIKeys` includes the legacy field when not OAuth; the
    /// single-key branch of `getAPIKeyProvider` selects it statically).
    /// `base_url` is carried through verbatim.
    #[test]
    fn active_credential_resolves_single_legacy_api_key() {
        let svc = ChannelService::new();
        let mut row = row_supporting_gpt4();
        row.credentials = serde_json::json!({ "apiKey": "sk-single" });

        let snapshot = build_snapshot(&row, &svc);

        assert_eq!(snapshot.base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(snapshot.active_credential.as_deref(), Some("sk-single"));
    }

    /// `apiKeys` multi-key with one entry in `disabledApiKeys` (Go
    /// `GetEnabledAPIKeys` filters disabled keys): the surviving key is
    /// selected via the single-enabled-key static branch.
    #[test]
    fn active_credential_skips_disabled_api_keys() {
        let svc = ChannelService::new();
        let mut row = row_supporting_gpt4();
        row.credentials = serde_json::json!({ "apiKeys": ["sk-a", "sk-b"] });
        row.disabled_api_keys = serde_json::json!([{ "key": "sk-a", "errorCode": 401 }]);

        let snapshot = build_snapshot(&row, &svc);

        assert_eq!(snapshot.active_credential.as_deref(), Some("sk-b"));
    }

    /// OAuth channel (Go `Credentials.IsOAuth()` true): no plaintext API key
    /// materializes at selection time — OAuth auth belongs to the
    /// transformer/oauth layer, mirroring Go's `IsOAuth` branch in
    /// `channel_llm.go`.
    #[test]
    fn active_credential_is_none_for_oauth_channel() {
        let svc = ChannelService::new();
        let mut row = row_supporting_gpt4();
        row.credentials = serde_json::json!({ "oauth": { "access_token": "tok-123" } });

        let snapshot = build_snapshot(&row, &svc);

        assert_eq!(snapshot.active_credential, None);
    }
}

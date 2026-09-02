//! RUST-P13-005 S08 — provider-quota GraphQL mutations.
//!
//! Ports the three provider-quota-related mutations from the Go gqlgen schema
//! (`conduit/internal/server/gql/system.graphql` lines 373-376) and their
//! resolvers (`conduit/internal/server/gql/system.resolvers.go` lines 184-262):
//!
//!   - `Mutation.checkProviderQuotas: Boolean!`
//!     Go resolver (line 237-245): if `providerQuotaService == nil` return
//!     `false` + error "provider quota service is not available"; otherwise
//!     call `providerQuotaService.ManualCheck(ctx)` (fire-and-forget) and
//!     return `true`.
//!   - `Mutation.resetChannelQuotaNow(channelID: ID!): Boolean!`
//!     Go resolver (line 248-262): scope-check `write:channels`; if
//!     `providerQuotaService == nil` return `false` + error; otherwise call
//!     `providerQuotaService.ResetChannelQuotaNow(ctx, channelID.ID)` and
//!     return `true` on success.
//!   - `Mutation.updateQuotaEnforcementSettings(input: UpdateQuotaEnforcementSettingsInput!): Boolean!`
//!     Go resolver (line 185-207): read current settings via
//!     `systemService.QuotaEnforcementSettings`, merge the non-nil input
//!     fields (Enabled / Mode) over the current values, then write back via
//!     `systemService.SetQuotaEnforcementSettings` and return `true`.
//!
//! The input type mirrors `conduit/internal/server/gql/system.graphql:177-180`
//! and `tests/contracts/admin_graphql_schema.graphql:9517-9520`:
//! ```graphql
//! input UpdateQuotaEnforcementSettingsInput {
//!   enabled: Boolean
//!   mode: QuotaEnforcementMode
//! }
//! ```
//! Both fields are nullable because the Go resolver applies a partial merge
//! (`if input.Enabled != nil` / `if input.Mode != nil`).
//!
//! ## Service wiring
//!
//! The admin-graphql crate cannot depend on `conduit-services` (the
//! `ProviderQuotaService` there only exposes the pure status read/write slice;
//! `ManualCheck` and `ResetChannelQuotaNow` are HTTP/DB-bound and have not been
//! ported yet). Instead we define a `QuotaMutationServices` trait that captures
//! the three operations the resolvers need. The host application wires the real
//! service implementation at schema-build time; the unit tests use an in-memory
//! fake. This mirrors the dependency-injection pattern the Go resolver uses via
//! the `ProviderQuotaService`/`SystemService` struct fields on `ResolverRoot`.

use std::{str::FromStr, sync::Arc};

use async_graphql::{Context, InputObject, Object, SimpleObject};
use rust_decimal::Decimal;

use crate::apikey::{
    APIKey, APIKeyStatus, CreateAPIKeyInput, UpdateAPIKeyInput, UpdateAPIKeyProfilesInput,
    api_key_access_scope, apikey_mutation_services,
};
use crate::channel::{
    Channel, CreateChannelInput, UpdateChannelInput, channel_mutation_services,
    validate_and_normalize_channel_settings_input,
};
use crate::model::{CreateModelInput, Model, UpdateModelInput, model_mutation_services};
use crate::profile_template::{
    APIKeyProfileTemplate, CreateAPIKeyProfileTemplateInput, LoadApiKeyProfileTemplateInput,
    UpdateAPIKeyProfileTemplateInput, profile_template_mutation_services,
};
use crate::project::{
    CreateProjectInput, Project, ProjectStatus, UpdateProjectInput, UpdateProjectProfilesInput,
    project_mutation_services,
};

use crate::prompt::{
    CreatePromptInput, CreatePromptProtectionRuleInput, Prompt, PromptProtectionRule,
    PromptProtectionRulePreviewInput, PromptProtectionRulePreviewResult, PromptStatus,
    UpdatePromptInput, UpdatePromptProtectionRuleInput, prompt_mutation_services,
    prompt_protection_rule_mutation_services,
};
use crate::role::{
    CreateRoleInput, Role, RoleConnectionArgs, RoleWhereInput, UpdateRoleInput,
    role_mutation_services, role_query_services,
};
use crate::scalars::QuotaEnforcementMode;
use crate::simple_group::{
    AssignSimpleGroupUsersInput, CreateSimpleGroupInput, SimpleGroup, UpdateSimpleGroupInput,
    UpdateSimpleGroupModelsInput, UpdateSimpleGroupPriceInput, simple_group_services,
};
use crate::system::{
    CompleteOnboardingInput, ProxyPreset, RetryPolicy, SaveProxyPresetInput, StoragePolicy,
    SystemModelSettings, UpdateBrandSettingsInput, UpdateDefaultDataStorageInput,
    UpdateRetryPolicyInput, UpdateSecuritySettingsInput, UpdateStoragePolicyInput,
    UpdateSystemGeneralSettingsInput, UpdateSystemModelSettingsInput,
    UpdateUserAgentPassThroughSettingsInput, merge_security_settings, system_settings_services,
};
use crate::user::{
    AddUserToProjectInput, CreateUserInput, RemoveUserFromProjectInput, UpdateProjectUserInput,
    UpdateUserInput, User, UserProject, UserStatus, user_mutation_services,
    validate_create_user_input, validate_update_user_input,
};

fn require_owner_for_internal_admin_scope<'a>(
    ctx: &Context<'_>,
    scopes: impl IntoIterator<Item = &'a String>,
) -> Result<(), String> {
    let requests_internal_admin = scopes
        .into_iter()
        .any(|scope| scope == conduit_auth::scopes::slug::SYSTEM_ADMIN);
    if !requests_internal_admin {
        return Ok(());
    }

    let is_owner = ctx
        .data_opt::<conduit_auth::RequestContext>()
        .and_then(|request| request.principal.as_ref())
        .is_some_and(|principal| principal.is_owner);
    if is_owner {
        Ok(())
    } else {
        Err("authz: only an owner may grant the system:admin scope".to_string())
    }
}

async fn guard_role_grants(
    ctx: &Context<'_>,
    role_ids: Option<&[async_graphql::ID]>,
) -> Result<(), String> {
    let Some(role_ids) = role_ids.filter(|ids| !ids.is_empty()) else {
        return Ok(());
    };
    let access = crate::policy::AdminAccessScope::from_graphql_context(
        ctx,
        conduit_auth::scopes::slug::WRITE_USERS,
    )
    .map_err(|error| error.to_string())?;
    let services = role_query_services(ctx)?;
    for role_id in role_ids {
        let connection = services
            .roles_with_access(
                &access,
                RoleConnectionArgs {
                    where_filter: Some(RoleWhereInput {
                        id: Some(role_id.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let wanted = canonical_node_id(role_id.as_str(), "Role");
        let role = connection
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.node)
            .find(|role| canonical_node_id(role.id.as_str(), "Role") == wanted)
            .ok_or_else(|| "permission denied: role is outside the authorized scope".to_owned())?;
        crate::policy::guard_scope_grant(ctx, None, role.scopes.iter().flatten())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn canonical_node_id(raw: &str, expected_type: &str) -> String {
    crate::node::parse_guid(raw)
        .ok()
        .filter(|guid| guid.typ == expected_type)
        .map_or_else(|| raw.to_owned(), |guid| guid.id.to_string())
}

// ---------------------------------------------------------------------------
// Input + merge DTO
// ---------------------------------------------------------------------------

/// GraphQL input object mirroring
/// `input UpdateQuotaEnforcementSettingsInput` (Go system.graphql:177-180 /
/// snapshot line 9517-9520).
///
/// Both fields are nullable to match the Go partial-merge semantics: a `None`
/// field means "leave the current value untouched" (Go: `if input.X != nil`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateQuotaEnforcementSettingsInput")]
pub struct UpdateQuotaEnforcementSettingsInput {
    pub enabled: Option<bool>,
    pub mode: Option<QuotaEnforcementMode>,
}

/// GraphQL `type QuotaEnforcementSettings { enabled: Boolean! mode:
/// QuotaEnforcementMode! }` (snapshot lines 9512-9515) and the in-memory
/// representation of the persisted `biz.QuotaEnforcementSettings`
/// (system.go:203-209). The update-mutation resolver reads the current value,
/// applies the partial merge, then writes it back; the `quotaEnforcementSettings`
/// query resolver (crate::quota_ext) returns it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "QuotaEnforcementSettings")]
pub struct QuotaEnforcementSettings {
    pub enabled: bool,
    pub mode: QuotaEnforcementMode,
}

impl Default for QuotaEnforcementSettings {
    /// Mirrors Go `defaultQuotaEnforcementSettings` (system_default.go:76-79):
    /// `Enabled: false, Mode: QuotaEnforcementModeExhaustedOnly`.
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        Self {
            enabled: false,
            mode: QuotaEnforcementMode::ExhaustedOnly,
        }
    }
}

/// Applies the Go resolver's partial merge (system.resolvers.go:190-199):
/// every `Some` input field overrides the current value; `None` fields are
/// left untouched.
///
/// This is a pure helper so the merge logic can be unit-tested independently
/// of any service implementation.
pub fn merge_quota_enforcement_settings(
    current: QuotaEnforcementSettings,
    input: &UpdateQuotaEnforcementSettingsInput,
) -> QuotaEnforcementSettings {
    QuotaEnforcementSettings {
        enabled: input.enabled.unwrap_or(current.enabled),
        mode: input.mode.unwrap_or(current.mode),
    }
}

// ---------------------------------------------------------------------------
// Service trait (host-injected)
// ---------------------------------------------------------------------------

/// Error surface for the quota-mutation services. The GraphQL layer surfaces
/// these as field errors; the message mirrors the Go `fmt.Errorf("...: %w")`
/// prefixes so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum QuotaMutationError {
    #[error("provider quota service is not available")]
    ServiceUnavailable,
    #[error("permission denied: requires write:channels scope")]
    MissingWriteChannelsScope,
    #[error("failed to read current quota enforcement settings: {0}")]
    ReadCurrent(String),
    #[error("failed to update quota enforcement settings: {0}")]
    Update(String),
    #[error("failed to check provider quota: {0}")]
    Check(String),
    #[error("failed to reset channel quota: {0}")]
    Reset(String),
}

/// Result of a one-channel provider balance probe. A failed capability probe
/// is returned as `success = false` instead of a GraphQL transport error so
/// the channel dialog can explain what is currently supported.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelQuotaProbeResult {
    pub success: bool,
    pub adapter: Option<String>,
    pub message: String,
    pub currency: Option<String>,
    pub total: Option<String>,
    pub used: Option<String>,
    pub remaining: Option<String>,
    pub balance_source: Option<String>,
    pub requires_pat: bool,
    pub unlimited: bool,
    pub unlimited_key_count: i32,
    pub key_count: i32,
    pub verified: bool,
    pub verified_at: Option<String>,
}

/// A NEW API station's effective upstream price for one model, normalized to
/// USD. `quality` is `exact`, `estimated`, or `unsupported`; callers must not
/// silently persist unsupported rows.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct NewApiModelPricingProbe {
    pub model_id: String,
    pub billing_kind: String,
    pub quality: String,
    pub group_ratio: Option<String>,
    pub input_per_million: Option<String>,
    pub output_per_million: Option<String>,
    pub cache_read_per_million: Option<String>,
    pub cache_write_per_million: Option<String>,
    pub flat_per_request: Option<String>,
    pub reason: Option<String>,
}

/// Preview returned by `probeNewApiPricing`. It never mutates channel model
/// prices; the administrator must explicitly apply and save the preview.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct NewApiPricingProbeResult {
    pub source: String,
    pub source_endpoint: String,
    pub fetched_at: String,
    pub pricing_version: Option<String>,
    pub account_group: Option<String>,
    pub effective_groups: Vec<String>,
    pub key_count: i32,
    pub matched_key_count: i32,
    pub warnings: Vec<String>,
    pub models: Vec<NewApiModelPricingProbe>,
}

/// Trait the host wires to back the three provider-quota mutations. Each method
/// corresponds to one Go resolver branch; the signatures are synchronous to the
/// resolver layer (the trait itself is async-trait-shaped via async-graphql's
/// `async` resolver methods).
///
/// This is intentionally minimal: it captures only the operations the three
/// mutations need. A future task will port the full `ProviderQuotaService` and
/// implement this trait concretely.
#[async_trait::async_trait]
pub trait QuotaMutationServices: Send + Sync {
    /// Mirrors Go `ProviderQuotaService.ManualCheck` (provider_quota.go:437):
    /// fire-and-forget force of an immediate quota re-check across all
    /// relevant channels. Returns `Ok(())` on success.
    ///
    /// Implementations MUST match the Go resolver's nil-guard: if the
    /// underlying service is not available, return
    /// [`QuotaMutationError::ServiceUnavailable`].
    async fn manual_check(&self) -> Result<(), QuotaMutationError>;

    /// Run a one-off capability probe. This never opts the channel into the
    /// scheduler; it only persists and returns a snapshot for administrator
    /// comparison with the upstream NEW API dashboard.
    async fn probe_channel_quota(
        &self,
        channel_id: &str,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<&str>,
    ) -> Result<ChannelQuotaProbeResult, QuotaMutationError>;

    /// Re-run the probe and, only on success, mark the adapter as verified for
    /// subsequent scheduled refreshes.
    async fn confirm_channel_quota_probe(
        &self,
        channel_id: &str,
    ) -> Result<ChannelQuotaProbeResult, QuotaMutationError>;

    /// Read the effective pricing advertised by a NEW API upstream. The PAT
    /// identifies the upstream account; supplied credentials are persisted
    /// only after the upstream accepts them.
    async fn probe_new_api_pricing(
        &self,
        channel_id: &str,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<&str>,
    ) -> Result<NewApiPricingProbeResult, QuotaMutationError>;

    /// Mirrors Go `ProviderQuotaService.ResetChannelQuotaNow`
    /// (provider_quota.go:442): attempt a banked-reset-credit redemption for
    /// the given channel (codex-only in Go). The caller has already verified
    /// the `write:channels` scope.
    ///
    /// `channel_id` is the raw ID string the GraphQL `ID!` scalar carried; Go
    /// extracts `channelID.ID` (an int) before calling the service. Concrete
    /// implementers parse it to the DB key type.
    async fn reset_channel_quota_now(&self, channel_id: &str) -> Result<(), QuotaMutationError>;

    /// Mirrors Go `SystemService.QuotaEnforcementSettings`
    /// (system.go:1535-1555): read the persisted settings (or the default on
    /// not-found). Never returns an error for the not-found case — Go returns
    /// the default in that branch.
    async fn quota_enforcement_settings(
        &self,
    ) -> Result<QuotaEnforcementSettings, QuotaMutationError>;

    /// Mirrors Go `SystemService.SetQuotaEnforcementSettings`
    /// (system.go:1570-1585): validate the mode, marshal, persist. Returns
    /// `QuotaMutationError::Update` on validation/IO failure.
    async fn set_quota_enforcement_settings(
        &self,
        settings: QuotaEnforcementSettings,
    ) -> Result<(), QuotaMutationError>;
}

// ---------------------------------------------------------------------------
// Mutation root
// ---------------------------------------------------------------------------

/// GraphQL `Mutation` root for the admin schema. Holds an injected
/// [`QuotaMutationServices`] handle the host wires at schema-build time.
///
/// Resolvers obtain the service via `ctx.data()` (async-graphql's data bag),
/// mirroring how the Go resolver reaches `r.providerQuotaService` /
/// `r.systemService` on the embedded `ResolverRoot`. This avoids a field on
/// `MutationRoot` itself, which async-graphql requires to be `Default`-able for
/// schema building — we instead inject the service into the schema's data at
/// build time.
pub struct MutationRoot;

fn quota_rules_total(rules: &[crate::billing::SubscriptionQuotaRuleInput]) -> Option<String> {
    rules
        .iter()
        .try_fold(Decimal::ZERO, |total, rule| {
            Decimal::from_str(rule.allowance.trim())
                .ok()
                .map(|amount| total + amount)
        })
        .map(|total| total.normalize().to_string())
}

async fn finish_billing_audit<T>(
    services: &Arc<dyn crate::billing::BillingServices>,
    mut audit: crate::billing::CommercialOperationAudit,
    operation_result: Result<T, crate::billing::BillingError>,
) -> Result<T, String> {
    match &operation_result {
        Ok(_) => audit.result = "success".into(),
        Err(error) => {
            audit.result = "failure".into();
            audit.error_message = Some(error.to_string());
        }
    }

    let operation = audit.operation.clone();
    let actor_type = audit.actor_type.clone();
    let actor_id = audit.actor_id.clone().unwrap_or_else(|| "unknown".into());
    if let Err(error) = services.record_commercial_operation_audit(audit).await {
        // The business transaction may already be committed. Surfacing the
        // audit error as the mutation result would falsely imply a rollback,
        // so retain the original result and emit an operationally visible log.
        tracing::error!(
            %operation,
            %actor_type,
            %actor_id,
            %error,
            "failed to persist commercial operation audit"
        );
    }

    operation_result.map_err(|error| error.to_string())
}

async fn lifecycle_subscription_mutation(
    ctx: &Context<'_>,
    subscription_id: async_graphql::ID,
    operation: &'static str,
) -> Result<crate::billing::UserSubscription, String> {
    let services = crate::billing::billing_services(ctx)?;
    let mut audit = crate::billing::CommercialOperationAudit::for_request(ctx, operation);
    audit.subscription_id = Some(subscription_id.to_string());
    let result = match operation {
        "pause_user_subscription" => {
            services
                .pause_user_subscription(subscription_id.as_str())
                .await
        }
        "resume_user_subscription" => {
            services
                .resume_user_subscription(subscription_id.as_str())
                .await
        }
        "cancel_user_subscription" => {
            services
                .cancel_user_subscription(subscription_id.as_str())
                .await
        }
        "renew_user_subscription" => {
            services
                .renew_user_subscription(subscription_id.as_str())
                .await
        }
        _ => unreachable!("unsupported subscription lifecycle operation"),
    };
    if let Ok(subscription) = &result {
        audit.target_user_id = Some(subscription.user_id.to_string());
        audit.target_project_id = subscription
            .project_id
            .as_ref()
            .map(|project_id| project_id.to_string());
        audit.plan_id = Some(subscription.plan.id.to_string());
    }
    finish_billing_audit(&services, audit, result).await
}

#[Object]
impl MutationRoot {
    /// Switch the product projection. Authorization is owner-only in the
    /// production authz extension; this resolver only persists the setting.
    async fn update_product_experience_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::product_experience::UpdateProductExperienceSettingsInput,
    ) -> Result<crate::product_experience::ProductExperienceSettings, String> {
        crate::product_experience::product_experience_services(ctx)?
            .update_settings(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn grant_user_credit(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::GrantUserCreditInput,
    ) -> Result<crate::billing::UserBalance, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "grant_user_credit");
        audit.target_user_id = Some(input.user_id.to_string());
        audit.amount = Some(input.amount.clone());
        audit.currency = Some(
            input
                .currency
                .clone()
                .unwrap_or_else(|| conduit_core::objects::money::STATION_CREDIT_CODE.into()),
        );
        audit.idempotency_key = Some(input.idempotency_key.clone());
        let result = services.grant_user_credit(input).await;
        finish_billing_audit(&services, audit, result).await
    }

    async fn grant_project_credit(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::GrantProjectCreditInput,
    ) -> Result<crate::billing::ProjectBalance, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "grant_project_credit");
        audit.target_project_id = Some(input.project_id.to_string());
        audit.amount = Some(input.amount.clone());
        audit.currency = Some(conduit_core::objects::money::STATION_CREDIT_CODE.into());
        audit.idempotency_key = Some(input.idempotency_key.clone());
        let result = services.grant_project_credit(input).await;
        finish_billing_audit(&services, audit, result).await
    }

    async fn create_credit_redemption_codes(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::CreateCreditRedemptionCodesInput,
    ) -> Result<crate::billing::CreateCreditRedemptionCodesPayload, String> {
        crate::billing::validate_create_credit_redemption_codes_input(&input)
            .map_err(|error| error.to_string())?;
        let services = crate::billing::billing_services(ctx)?;
        let actor = crate::billing::CreditRedemptionActor::for_request(ctx)?;
        let mut audit = crate::billing::CommercialOperationAudit::for_request(
            ctx,
            "create_credit_redemption_codes",
        );
        audit.amount = Some(input.amount.clone());
        audit.currency = Some(conduit_core::objects::money::STATION_CREDIT_CODE.into());
        let result = services.create_credit_redemption_codes(actor, input).await;
        finish_billing_audit(&services, audit, result).await
    }

    async fn revoke_credit_redemption_code(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<crate::billing::CreditRedemptionCode, String> {
        let services = crate::billing::billing_services(ctx)?;
        let actor = crate::billing::CreditRedemptionActor::for_request(ctx)?;
        let audit = crate::billing::CommercialOperationAudit::for_request(
            ctx,
            "revoke_credit_redemption_code",
        );
        let result = services
            .revoke_credit_redemption_code(actor, id.as_str())
            .await;
        finish_billing_audit(&services, audit, result).await
    }

    async fn redeem_credit_code(
        &self,
        ctx: &Context<'_>,
        code: String,
    ) -> Result<crate::billing::CreditRedemptionReceipt, String> {
        let current = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?;
        let project_id = crate::policy::request_context(ctx)
            .and_then(|request| request.project_id.as_deref())
            .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
        let code = crate::billing::normalize_credit_redemption_code(&code)
            .map_err(|error| error.to_string())?;
        let services = crate::billing::billing_services(ctx)?;
        let actor = crate::billing::CreditRedemptionActor::for_request(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "redeem_credit_code");
        audit.target_user_id = Some(current.user_id.to_string());
        audit.target_project_id = Some(project_id.to_string());
        let result = services
            .redeem_credit_code(actor, &current.user_id.to_string(), project_id, &code)
            .await;
        if let Ok(receipt) = &result {
            audit.amount = Some(receipt.amount.clone());
            audit.currency = Some(receipt.currency.clone());
        }
        finish_billing_audit(&services, audit, result).await
    }

    async fn create_subscription_plan(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::CreateSubscriptionPlanInput,
    ) -> Result<crate::billing::SubscriptionPlan, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "create_subscription_plan");
        audit.plan_name = Some(input.name.clone());
        audit.amount = quota_rules_total(&input.quota_rules);
        audit.currency = Some(conduit_core::objects::money::STATION_CREDIT_CODE.into());
        let result = services.create_subscription_plan(input).await;
        if let Ok(plan) = &result {
            audit.plan_id = Some(plan.id.to_string());
        }
        finish_billing_audit(&services, audit, result).await
    }

    async fn update_subscription_plan(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::UpdateSubscriptionPlanInput,
    ) -> Result<crate::billing::SubscriptionPlan, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "update_subscription_plan");
        audit.plan_id = Some(input.id.to_string());
        audit.plan_name = Some(input.name.clone());
        audit.amount = quota_rules_total(&input.quota_rules);
        audit.currency = Some(conduit_core::objects::money::STATION_CREDIT_CODE.into());
        let result = services.update_subscription_plan(input).await;
        finish_billing_audit(&services, audit, result).await
    }

    async fn assign_user_subscription(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::AssignUserSubscriptionInput,
    ) -> Result<crate::billing::UserSubscription, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit =
            crate::billing::CommercialOperationAudit::for_request(ctx, "assign_user_subscription");
        audit.target_user_id = Some(input.user_id.to_string());
        audit.target_project_id = Some(input.project_id.as_str().to_owned());
        audit.plan_id = Some(input.plan_id.to_string());
        audit.idempotency_key = Some(input.idempotency_key.clone());
        let result = services.assign_user_subscription(input).await;
        if let Ok(subscription) = &result {
            audit.subscription_id = Some(subscription.id.to_string());
        }
        finish_billing_audit(&services, audit, result).await
    }

    async fn refresh_subscription_allowance(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "subscriptionID")] subscription_id: async_graphql::ID,
    ) -> Result<crate::billing::UserSubscription, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit = crate::billing::CommercialOperationAudit::for_request(
            ctx,
            "refresh_subscription_allowance",
        );
        audit.subscription_id = Some(subscription_id.to_string());
        let result = services
            .refresh_subscription_allowance(subscription_id.as_str())
            .await;
        if let Ok(subscription) = &result {
            audit.target_user_id = Some(subscription.user_id.to_string());
            audit.target_project_id = subscription
                .project_id
                .as_ref()
                .map(|project_id| project_id.as_str().to_owned());
            audit.plan_id = Some(subscription.plan.id.to_string());
        }
        finish_billing_audit(&services, audit, result).await
    }

    async fn pause_user_subscription(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "subscriptionID")] subscription_id: async_graphql::ID,
    ) -> Result<crate::billing::UserSubscription, String> {
        lifecycle_subscription_mutation(ctx, subscription_id, "pause_user_subscription").await
    }

    async fn resume_user_subscription(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "subscriptionID")] subscription_id: async_graphql::ID,
    ) -> Result<crate::billing::UserSubscription, String> {
        lifecycle_subscription_mutation(ctx, subscription_id, "resume_user_subscription").await
    }

    async fn cancel_user_subscription(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "subscriptionID")] subscription_id: async_graphql::ID,
    ) -> Result<crate::billing::UserSubscription, String> {
        lifecycle_subscription_mutation(ctx, subscription_id, "cancel_user_subscription").await
    }

    async fn renew_user_subscription(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "subscriptionID")] subscription_id: async_graphql::ID,
    ) -> Result<crate::billing::UserSubscription, String> {
        lifecycle_subscription_mutation(ctx, subscription_id, "renew_user_subscription").await
    }

    async fn set_subscription_auto_renew(
        &self,
        ctx: &Context<'_>,
        input: crate::billing::SetSubscriptionAutoRenewInput,
    ) -> Result<crate::billing::UserSubscription, String> {
        let services = crate::billing::billing_services(ctx)?;
        let mut audit = crate::billing::CommercialOperationAudit::for_request(
            ctx,
            "set_subscription_auto_renew",
        );
        audit.subscription_id = Some(input.subscription_id.to_string());
        let result = services.set_subscription_auto_renew(input).await;
        finish_billing_audit(&services, audit, result).await
    }

    /// Add or update a route from one public SKU to an upstream deployment.
    async fn upsert_model_route(
        &self,
        ctx: &Context<'_>,
        input: crate::commercialization::UpsertModelRouteInput,
    ) -> Result<crate::commercialization::ModelRoute, String> {
        crate::commercialization::commercialization_services(ctx)?
            .upsert_model_route(input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Atomically create a public model and its routes to discovered upstream deployments.
    async fn create_public_model_with_routes(
        &self,
        ctx: &Context<'_>,
        input: crate::commercialization::CreatePublicModelWithRoutesInput,
    ) -> Result<crate::commercialization::CreatePublicModelWithRoutesPayload, String> {
        crate::commercialization::commercialization_services(ctx)?
            .create_public_model_with_routes(input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Apply a previously reviewed route-to-alias preview with optimistic locking.
    async fn apply_channel_model_mappings(
        &self,
        ctx: &Context<'_>,
        input: crate::commercialization::ApplyChannelModelMappingsInput,
    ) -> Result<crate::commercialization::ChannelModelMappingPreview, String> {
        crate::commercialization::commercialization_services(ctx)?
            .apply_channel_model_mappings(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn set_channel_model_mapping_automation(
        &self,
        ctx: &Context<'_>,
        input: crate::commercialization::SetChannelModelMappingAutomationInput,
    ) -> Result<crate::commercialization::ChannelModelMappingAutomationSettings, String> {
        crate::commercialization::commercialization_services(ctx)?
            .set_channel_model_mapping_automation(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn create_price_book(
        &self,
        ctx: &Context<'_>,
        input: crate::commercialization::CreatePriceBookInput,
    ) -> Result<crate::commercialization::PriceBook, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        crate::commercialization::commercialization_services(ctx)?
            .create_price_book(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn create_retail_price_change_set(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "priceBookID")] price_book_id: async_graphql::ID,
    ) -> Result<crate::change_set::ChangeSet, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .create_retail_price_change_set(actor_user_id, price_book_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn create_provider_price_change_set(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<crate::change_set::ChangeSet, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .create_provider_price_change_set(actor_user_id, channel_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn save_retail_price_change_set_item(
        &self,
        ctx: &Context<'_>,
        input: crate::change_set::SaveRetailPriceChangeSetItemInput,
    ) -> Result<crate::change_set::ChangeSetItem, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .save_retail_price_change_set_item(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn submit_change_set(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<crate::change_set::ChangeSet, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .submit_change_set(actor_user_id, id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn approve_change_set(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        review_note: Option<String>,
    ) -> Result<crate::change_set::ChangeSet, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .approve_change_set(actor_user_id, id, review_note)
            .await
            .map_err(|error| error.to_string())
    }

    async fn reject_change_set(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        review_note: Option<String>,
    ) -> Result<crate::change_set::ChangeSet, String> {
        let actor_user_id = ctx
            .data::<crate::me::CurrentUser>()
            .map_err(|_| "authentication required".to_string())?
            .user_id;
        crate::change_set::change_set_services(ctx)?
            .reject_change_set(actor_user_id, id, review_note)
            .await
            .map_err(|error| error.to_string())
    }

    /// Create a simple-mode Group and its normalized commercial references in
    /// one service transaction.
    async fn create_simple_group(
        &self,
        ctx: &Context<'_>,
        input: CreateSimpleGroupInput,
    ) -> Result<SimpleGroup, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        simple_group_services(ctx)?
            .create_simple_group(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Update the complete simple-mode bundle without exposing the enterprise
    /// objects that implement it.
    async fn update_simple_group(
        &self,
        ctx: &Context<'_>,
        input: UpdateSimpleGroupInput,
    ) -> Result<SimpleGroup, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        simple_group_services(ctx)?
            .update_simple_group(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Move the selected users' unique personal Projects into one normalized
    /// Simple Group and apply that Group's base commercial policy.
    async fn assign_simple_group_users(
        &self,
        ctx: &Context<'_>,
        input: AssignSimpleGroupUsersInput,
    ) -> Result<SimpleGroup, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        simple_group_services(ctx)?
            .assign_simple_group_users(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn update_simple_group_models(
        &self,
        ctx: &Context<'_>,
        input: UpdateSimpleGroupModelsInput,
    ) -> Result<SimpleGroup, String> {
        simple_group_services(ctx)?
            .update_simple_group_models(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn update_simple_group_price(
        &self,
        ctx: &Context<'_>,
        input: UpdateSimpleGroupPriceInput,
    ) -> Result<SimpleGroup, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        simple_group_services(ctx)?
            .update_simple_group_price(actor_user_id, input)
            .await
            .map_err(|error| error.to_string())
    }

    /// Archive a normalized Simple Group. The service intentionally exposes
    /// no physical-delete switch so member and commercial history stays
    /// traceable.
    async fn delete_simple_group(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<SimpleGroup, String> {
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        simple_group_services(ctx)?
            .delete_simple_group(actor_user_id, id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    async fn restore(
        &self,
        ctx: &Context<'_>,
        file: async_graphql::Upload,
        input: crate::backup_ext::RestoreOptionsInput,
    ) -> Result<crate::backup_ext::RestorePayload, String> {
        use std::io::Read;

        let upload = file.value(ctx).map_err(|error| error.to_string())?;
        let mut data = Vec::new();
        upload
            .into_read()
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        crate::backup_ext::backup_ext_services(ctx)?
            .restore(data, input)
            .await
            .map_err(|error| error.to_string())?;
        Ok(crate::backup_ext::RestorePayload {
            success: true,
            message: Some("Restore completed successfully".to_string()),
        })
    }

    async fn trigger_auto_backup(
        &self,
        ctx: &Context<'_>,
    ) -> Result<crate::backup_ext::TriggerBackupPayload, String> {
        crate::backup_ext::backup_ext_services(ctx)?
            .trigger_auto_backup()
            .await
            .map_err(|error| error.to_string())?;
        Ok(crate::backup_ext::TriggerBackupPayload {
            success: true,
            message: Some("Backup completed successfully".to_string()),
        })
    }
    async fn trigger_gc_cleanup(
        &self,
        ctx: &Context<'_>,
        input: crate::system_operations_ext::TriggerGcCleanupInput,
    ) -> Result<bool, String> {
        crate::system_operations_ext::system_operations_services(ctx)
            .map_err(|error| error.to_string())?
            .trigger_gc_cleanup(input)
            .await
            .map_err(|error| error.to_string())
    }

    async fn clear_cache(
        &self,
        ctx: &Context<'_>,
        input: crate::system_operations_ext::ClearCacheInput,
    ) -> Result<crate::system_operations_ext::ClearCachePayload, String> {
        crate::system_operations_ext::system_operations_services(ctx)
            .map_err(|error| error.to_string())?
            .clear_cache(input)
            .await
            .map_err(|error| error.to_string())
    }
    // ----- model_ext slice (GAP-B): bulk model ops + channel model prices -----
    // Delegate bodies pasted from `model_ext.rs` wiring doc. The `#[Object]`
    // macro can't be split across modules, so these live here in the single
    // MutationRoot block and call the `model_ext` service helper + trait.

    /// `Mutation.bulkCreateModels` — Mirrors Go `model.resolvers.go:31-34`.
    async fn bulk_create_models(
        &self,
        ctx: &Context<'_>,
        inputs: Vec<crate::model::CreateModelInput>,
    ) -> Result<Vec<crate::model::Model>, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.bulk_create_models(inputs)
            .await
            .map_err(|e| e.to_string())
    }

    /// `Mutation.updateModelStatus` — Mirrors Go `model.resolvers.go:50-58`
    /// (returns `true` on success).
    async fn update_model_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: crate::model::ModelStatus,
    ) -> Result<bool, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.update_model_status(id, status)
            .await
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// `Mutation.bulkArchiveModels` — Mirrors Go `model.resolvers.go:60-69`.
    async fn bulk_archive_models(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.bulk_archive_models(ids)
            .await
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// `Mutation.bulkDisableModels` — Mirrors Go `model.resolvers.go:71-80`.
    async fn bulk_disable_models(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.bulk_disable_models(ids)
            .await
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// `Mutation.bulkEnableModels` — Mirrors Go `model.resolvers.go:82-91`.
    async fn bulk_enable_models(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.bulk_enable_models(ids).await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// `Mutation.bulkDeleteModels` — Mirrors Go `model.resolvers.go:93-102`.
    async fn bulk_delete_models(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        s.bulk_delete_models(ids).await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Compatibility mutation. Formal prices remain unchanged until the
    /// staged provider-price ChangeSet is approved.
    async fn save_channel_model_prices(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelId")] channel_id: async_graphql::ID,
        input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<Vec<crate::model_ext::ChannelModelPrice>, String> {
        let s = crate::model_ext::model_ext_services(ctx)?;
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        s.save_channel_model_prices(actor_user_id, channel_id, input)
            .await
            .map_err(|e| e.to_string())
    }

    /// Mirrors Go `Mutation.checkProviderQuotas` (system.resolvers.go:237-245):
    /// trigger a manual provider-quota re-check across all relevant channels.
    /// Returns `true` if the check was successfully kicked off.
    async fn check_provider_quotas(&self, ctx: &Context<'_>) -> Result<bool, String> {
        let services = quota_services(ctx)?;
        match services.manual_check().await {
            Ok(()) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Probe a single channel without enabling background checks.
    async fn probe_channel_quota(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        #[graphql(name = "newApiPAT")] new_api_pat: Option<String>,
        #[graphql(name = "newApiUserID")] new_api_user_id: Option<async_graphql::ID>,
    ) -> Result<ChannelQuotaProbeResult, String> {
        quota_services(ctx)?
            .probe_channel_quota(
                channel_id.as_str(),
                new_api_pat.as_deref(),
                new_api_user_id.as_ref().map(|id| id.as_str()),
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Confirm the probed values and enable the verified adapter for the
    /// scheduler. The host performs a second live request before persisting.
    async fn confirm_channel_quota_probe(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
    ) -> Result<ChannelQuotaProbeResult, String> {
        quota_services(ctx)?
            .confirm_channel_quota_probe(channel_id.as_str())
            .await
            .map_err(|error| error.to_string())
    }

    /// Preview effective NEW API upstream prices. This deliberately does not
    /// save them: applying the preview remains an explicit administrator step.
    async fn probe_new_api_pricing(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        #[graphql(name = "newApiPAT")] new_api_pat: Option<String>,
        #[graphql(name = "newApiUserID")] new_api_user_id: Option<async_graphql::ID>,
    ) -> Result<NewApiPricingProbeResult, String> {
        quota_services(ctx)?
            .probe_new_api_pricing(
                channel_id.as_str(),
                new_api_pat.as_deref(),
                new_api_user_id.as_ref().map(|id| id.as_str()),
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Mirrors Go `Mutation.resetChannelQuotaNow` (system.resolvers.go:248-262):
    /// clear/reset one channel's cached quota status immediately. The Go
    /// resolver scope-checks `write:channels` before delegating; scope
    /// enforcement is the host's responsibility (it injects a service impl
    /// that performs the check or the host middleware enforces it before the
    /// resolver runs). `channel_id` is the GraphQL `ID!` scalar value.
    async fn reset_channel_quota_now(
        &self,
        ctx: &Context<'_>,
        // Go SDL declares `channelID: ID!`; async-graphql's `ID` newtype maps
        // the Rust `String` to the GraphQL `ID!` scalar. The `name` attribute
        // pins the camelCase-with-acronym field name that `rename_all` would
        // otherwise mangle (CLAUDE.md all-caps acronym gotcha).
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = quota_services(ctx)?;
        match services.reset_channel_quota_now(channel_id.as_str()).await {
            Ok(()) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Mirrors Go `Mutation.updateQuotaEnforcementSettings`
    /// (system.resolvers.go:185-207): read-merge-write the quota enforcement
    /// config (enabled / mode). Returns `true` on success.
    async fn update_quota_enforcement_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateQuotaEnforcementSettingsInput,
    ) -> Result<bool, String> {
        let services = quota_services(ctx)?;
        let current = services
            .quota_enforcement_settings()
            .await
            .map_err(|err| err.to_string())?;
        let merged = merge_quota_enforcement_settings(current, &input);
        services
            .set_quota_enforcement_settings(merged)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // Channel CRUD (RUST-P12-001 S07). Contract: snapshot `type Mutation`
    // lines 789/792/795. Semantics and tests live in `crate::channel`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createChannel` (conduit.resolvers.go:156):
    /// `createChannel(input: CreateChannelInput!): Channel!` — delegates to
    /// `ChannelService.CreateChannel` (duplicate-name check, ent defaults,
    /// async channel reload).
    async fn create_channel(
        &self,
        ctx: &Context<'_>,
        input: CreateChannelInput,
    ) -> Result<Channel, String> {
        let mut input = input;
        validate_and_normalize_channel_settings_input(input.settings.as_mut())
            .map_err(|err| err.to_string())?;
        let services = channel_mutation_services(ctx)?;
        services
            .create_channel(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateChannel` (conduit.resolvers.go:171):
    /// `updateChannel(id: ID!, input: UpdateChannelInput!): Channel!` —
    /// delegates to `ChannelService.UpdateChannel` (duplicate-name check
    /// excluding self, partial merge). `id` is the GraphQL `ID!` scalar; Go
    /// decodes it into `objects.GUID` and passes `.ID` to the service.
    async fn update_channel(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateChannelInput,
    ) -> Result<Channel, String> {
        let mut input = input;
        validate_and_normalize_channel_settings_input(input.settings.as_mut())
            .map_err(|err| err.to_string())?;
        let services = channel_mutation_services(ctx)?;
        services
            .update_channel(id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteChannel` (conduit.resolvers.go:186):
    /// `deleteChannel(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_channel(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = channel_mutation_services(ctx)?;
        services
            .delete_channel(id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // Model CRUD (RUST-P12-001 S07, model slice). Contract: snapshot
    // `extend type Mutation` lines 9114/9116/9117. Semantics and tests
    // live in `crate::model`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createModel` (model.resolvers.go:27):
    /// `createModel(input: CreateModelInput!): Model!` — delegates to
    /// `ModelService.CreateModel` (settings validation, duplicate-modelID
    /// check, ent defaults type=chat / status=disabled).
    async fn create_model(
        &self,
        ctx: &Context<'_>,
        input: CreateModelInput,
    ) -> Result<Model, String> {
        let services = model_mutation_services(ctx)?;
        services
            .create_model(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateModel` (model.resolvers.go:37):
    /// `updateModel(id: ID!, input: UpdateModelInput!): Model!` — delegates
    /// to `ModelService.UpdateModel` (partial merge; NO duplicate check, Go
    /// parity). `id` is the GraphQL `ID!` scalar; Go decodes it into
    /// `objects.GUID` and passes `.ID` to the service.
    async fn update_model(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateModelInput,
    ) -> Result<Model, String> {
        let services = model_mutation_services(ctx)?;
        services
            .update_model(id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteModel` (model.resolvers.go:42):
    /// `deleteModel(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_model(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<bool, String> {
        let services = model_mutation_services(ctx)?;
        services
            .delete_model(id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // APIKey CRUD (RUST-P12-001 S07, apikey slice). Contract: snapshot
    // `type Mutation` lines 819-823. Semantics and tests live in
    // `crate::apikey`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createAPIKey` (conduit.resolvers.go:395):
    /// `createAPIKey(input: CreateAPIKeyInput!): APIKey!` — delegates to
    /// `APIKeyService.CreateAPIKey` (reject noauth "reserved", per-project
    /// duplicate LIVE-name probe — ARCHIVED keys still occupy their name,
    /// generate the prefix-`-`-hex key, column defaults).
    #[graphql(name = "createAPIKey")]
    async fn create_api_key(
        &self,
        ctx: &Context<'_>,
        input: CreateAPIKeyInput,
    ) -> Result<APIKey, String> {
        require_owner_for_internal_admin_scope(
            ctx,
            input.scopes.iter().flat_map(|scopes| scopes.iter()),
        )?;
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        let current_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        services
            .create_api_key(&scope, current_user_id, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateAPIKey` (conduit.resolvers.go:400):
    /// `updateAPIKey(id: ID!, input: UpdateAPIKeyInput!): APIKey!` —
    /// delegates to `APIKeyService.UpdateAPIKey` (user-type rejects non-empty
    /// scope mutations, noauth-type rejects any update, rename duplicate
    /// probe excluding self, service_account-only scope set/append/clear with
    /// clear-last precedence).
    #[graphql(name = "updateAPIKey")]
    async fn update_api_key(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateAPIKeyInput,
    ) -> Result<APIKey, String> {
        require_owner_for_internal_admin_scope(
            ctx,
            input
                .scopes
                .iter()
                .chain(input.append_scopes.iter())
                .flat_map(|scopes| scopes.iter()),
        )?;
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        services
            .update_api_key(&scope, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateAPIKeyStatus` (conduit.resolvers.go:405):
    /// `updateAPIKeyStatus(id: ID!, status: APIKeyStatus!): APIKey!` —
    /// delegates to `APIKeyService.UpdateAPIKeyStatus` (noauth rejected; NO
    /// transition restriction so archived keys can be re-enabled).
    #[graphql(name = "updateAPIKeyStatus")]
    async fn update_api_key_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: APIKeyStatus,
    ) -> Result<APIKey, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        services
            .update_api_key_status(&scope, id.as_str(), status)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.rotateAPIKey` (conduit.resolvers.go:415):
    /// `rotateAPIKey(id: ID!): APIKey!` — delegates to
    /// `APIKeyService.RotateAPIKey` (noauth rejected; ONLY `key` changes —
    /// status/name/scopes/profiles preserved).
    #[graphql(name = "rotateAPIKey")]
    async fn rotate_api_key(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<APIKey, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        services
            .rotate_api_key(&scope, id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateAPIKeyProfiles` (conduit.resolvers.go:410):
    /// `updateAPIKeyProfiles(id: ID!, input: UpdateAPIKeyProfilesInput!): APIKey!`
    /// — delegates to `APIKeyService.UpdateAPIKeyProfiles` (biz/api_key.go:503):
    /// noauth-type rejected; profile names unique (case-insensitive, non-empty);
    /// active profile must exist in the profiles list; filters/quota validated;
    /// SetProfiles; cache invalidated.
    #[graphql(name = "updateAPIKeyProfiles")]
    async fn update_api_key_profiles(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateAPIKeyProfilesInput,
    ) -> Result<APIKey, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        services
            .update_api_key_profiles(&scope, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.bulkDisableAPIKeys` (conduit.resolvers.go:420):
    /// `bulkDisableAPIKeys(ids: [ID!]!): Boolean!` — delegates to
    /// `APIKeyService.BulkDisableAPIKeys` (biz/api_key.go:802 →
    /// `bulkUpdateAPIKeyStatus`, biz/api_key.go:751): empty ids is a no-op;
    /// all ids must exist; NO id may be `noauth`-type; bulk SetStatus disabled.
    /// Returns `true` on success (Go resolver wraps the error into `false`).
    #[graphql(name = "bulkDisableAPIKeys")]
    async fn bulk_disable_api_keys(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_disable_api_keys(&scope, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkEnableAPIKeys` (conduit.resolvers.go:432):
    /// `bulkEnableAPIKeys(ids: [ID!]!): Boolean!` — same shape as
    /// [`Self::bulk_disable_api_keys`] with status `enabled`.
    #[graphql(name = "bulkEnableAPIKeys")]
    async fn bulk_enable_api_keys(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_enable_api_keys(&scope, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkArchiveAPIKeys` (conduit.resolvers.go:444):
    /// `bulkArchiveAPIKeys(ids: [ID!]!): Boolean!` — same shape as
    /// [`Self::bulk_disable_api_keys`] with status `archived`.
    #[graphql(name = "bulkArchiveAPIKeys")]
    async fn bulk_archive_api_keys(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = apikey_mutation_services(ctx)?;
        let scope = api_key_access_scope(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_archive_api_keys(&scope, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // Channel override templates (P-54).
    // -----------------------------------------------------------------

    async fn create_channel_override_template(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_override_template_ext::CreateChannelOverrideTemplateInput,
    ) -> Result<crate::channel_override_template_ext::ChannelOverrideTemplate, String> {
        let services =
            crate::channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        let user = crate::me::current_user(ctx)?;
        services
            .create(user.user_id, input)
            .await
            .map_err(|err| err.to_string())
    }

    async fn update_channel_override_template(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: crate::channel_override_template_ext::UpdateChannelOverrideTemplateInput,
    ) -> Result<crate::channel_override_template_ext::ChannelOverrideTemplate, String> {
        let services =
            crate::channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        let user = crate::me::current_user(ctx)?;
        services
            .update(user.user_id, id.to_string(), input)
            .await
            .map_err(|err| err.to_string())
    }

    async fn delete_channel_override_template(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services =
            crate::channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        let user = crate::me::current_user(ctx)?;
        services
            .delete(user.user_id, id.to_string())
            .await
            .map_err(|err| err.to_string())
    }

    async fn apply_channel_override_template(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_override_template_ext::ApplyChannelOverrideTemplateInput,
    ) -> Result<crate::channel_override_template_ext::ApplyChannelOverrideTemplatePayload, String>
    {
        let services =
            crate::channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        let user = crate::me::current_user(ctx)?;
        services
            .apply(user.user_id, input)
            .await
            .map_err(|err| err.to_string())
    }

    async fn clear_channel_override_templates(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_override_template_ext::ClearChannelOverrideTemplatesInput,
    ) -> Result<crate::channel_override_template_ext::ClearChannelOverrideTemplatesPayload, String>
    {
        let services =
            crate::channel_override_template_ext::channel_override_template_ext_services(ctx)?;
        services.clear(input).await.map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // APIKeyProfileTemplate CRUD (RUST-P12-001 S07, profile_template slice).
    // Contract: snapshot `type Mutation` lines 871-883. Semantics and tests
    // live in `crate::profile_template`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createApiKeyProfileTemplate`
    /// (`conduit.resolvers.go:653`): delegates to
    /// `APIKeyProfileTemplateService.CreateTemplate`
    /// (`biz/api_key_profile_template.go:34`): force `profile.Name =
    /// input.Name`, persist via `SetInput(input).SetProfile(profile)`, surface
    /// the friendly duplicate-name error on the unique constraint.
    #[graphql(name = "createApiKeyProfileTemplate")]
    async fn create_api_key_profile_template(
        &self,
        ctx: &Context<'_>,
        input: CreateAPIKeyProfileTemplateInput,
        profile: crate::apikey::APIKeyProfileInput,
    ) -> Result<APIKeyProfileTemplate, String> {
        let services = profile_template_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_API_KEYS,
        )
        .map_err(|error| error.to_string())?;
        services
            .create_api_key_profile_template_with_access(&access, input, profile)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateApiKeyProfileTemplate`
    /// (`conduit.resolvers.go:658`): delegates to
    /// `APIKeyProfileTemplateService.UpdateTemplate`
    /// (`biz/api_key_profile_template.go:114`): partial merge of name /
    /// description, optional profile replacement (profile.Name falls back to
    /// the existing template name when input.Name is nil), duplicate-name
    /// probe excluding self.
    #[graphql(name = "updateApiKeyProfileTemplate")]
    async fn update_api_key_profile_template(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateAPIKeyProfileTemplateInput,
        profile: Option<crate::apikey::APIKeyProfileInput>,
    ) -> Result<APIKeyProfileTemplate, String> {
        let services = profile_template_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_API_KEYS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_api_key_profile_template_with_access(&access, id.as_str(), input, profile)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteApiKeyProfileTemplate`
    /// (`conduit.resolvers.go:663`): delegates to
    /// `APIKeyProfileTemplateService.DeleteTemplate`
    /// (`biz/api_key_profile_template.go:156`): get-then-delete inside a
    /// transaction, returns the pre-delete snapshot.
    #[graphql(name = "deleteApiKeyProfileTemplate")]
    async fn delete_api_key_profile_template(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<APIKeyProfileTemplate, String> {
        let services = profile_template_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_API_KEYS,
        )
        .map_err(|error| error.to_string())?;
        services
            .delete_api_key_profile_template_with_access(&access, id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.loadApiKeyProfileTemplate`
    /// (`conduit.resolvers.go:668`): delegates to
    /// `APIKeyProfileTemplateService.LoadTemplate`
    /// (`biz/api_key_profile_template.go:181`): clone the template profile,
    /// resolve a name-conflict-safe variant, append it to the target APIKey's
    /// `profiles` list, and return the updated APIKey.
    #[graphql(name = "loadApiKeyProfileTemplate")]
    async fn load_api_key_profile_template(
        &self,
        ctx: &Context<'_>,
        input: LoadApiKeyProfileTemplateInput,
    ) -> Result<APIKey, String> {
        let services = profile_template_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_API_KEYS,
        )
        .map_err(|error| error.to_string())?;
        services
            .load_api_key_profile_template_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Project CRUD (RUST-P12-001 S07, project slice). Contract: snapshot
    // `type Mutation` lines 838-842. Semantics and tests live in
    // `crate::project`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createProject` (conduit.resolvers.go:512):
    /// `createProject(input: CreateProjectInput!): Project!` — delegates to
    /// `ProjectService.CreateProject` (duplicate-name check, three default
    /// project-level roles, creator-as-owner link).
    async fn create_project(
        &self,
        ctx: &Context<'_>,
        input: CreateProjectInput,
    ) -> Result<Project, String> {
        let services = project_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .create_project_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateProject` (conduit.resolvers.go:517):
    /// `updateProject(id: ID!, input: UpdateProjectInput!): Project!` —
    /// delegates to `ProjectService.UpdateProject` (partial merge of name /
    /// description; clearUsers wins over add/remove per Go if-else ordering).
    async fn update_project(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateProjectInput,
    ) -> Result<Project, String> {
        let services = project_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_project_with_access(&access, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateProjectStatus` (conduit.resolvers.go:522):
    /// `updateProjectStatus(id: ID!, status: ProjectStatus!): Project!` —
    /// delegates to `ProjectService.UpdateProjectStatus`.
    async fn update_project_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: ProjectStatus,
    ) -> Result<Project, String> {
        let services = project_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_project_status_with_access(&access, id.as_str(), status)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateProjectProfiles` (conduit.resolvers.go:527):
    /// `updateProjectProfiles(id: ID!, input: UpdateProjectProfilesInput!): Project!`
    /// — delegates to `ProjectService.UpdateProjectProfiles`
    /// (biz/project.go:235): validate the embedded profiles (unique non-empty
    /// names, valid activeProfile, valid channelTagsMatchMode) → SetProfiles +
    /// cache invalidation.
    async fn update_project_profiles(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateProjectProfilesInput,
    ) -> Result<Project, String> {
        let services = project_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_project_profiles_with_access(&access, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteProject` (conduit.resolvers.go:532):
    /// `deleteProject(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_project(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = project_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .delete_project_with_access(&access, id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // User CRUD (RUST-P12-001 S07, user slice). Contract: snapshot
    // `type Mutation` lines 823-826 + lines 839-841 (project-link mutations
    // delegate to biz.UserService). Semantics and tests live in
    // `crate::user`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createUser` (conduit.resolvers.go:456):
    /// `createUser(input: CreateUserInput!): User!` — delegates to
    /// `UserService.CreateUser` (hash password unless OIDC-only placeholder,
    /// SetNillableFirstName/LastName, SetEmail, SetScopes, AddRoleIDs).
    async fn create_user(&self, ctx: &Context<'_>, input: CreateUserInput) -> Result<User, String> {
        validate_create_user_input(&input).map_err(|err| err.to_string())?;
        // P-31: a non-owner may not create an owner or seed scopes they lack.
        crate::policy::guard_scope_grant(ctx, input.is_owner, input.scopes.iter().flatten())
            .map_err(|err| err.to_string())?;
        guard_role_grants(ctx, input.role_ids.as_deref()).await?;
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .create_user_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateUser` (conduit.resolvers.go:461):
    /// `updateUser(id: ID!, input: UpdateUserInput!): User!` — delegates
    /// to `UserService.UpdateUser` (partial merge).
    async fn update_user(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateUserInput,
    ) -> Result<User, String> {
        validate_update_user_input(&input).map_err(|err| err.to_string())?;
        // P-31: a non-owner may not self-promote to owner or grant scopes they
        // do not already hold.
        crate::policy::guard_scope_grant(
            ctx,
            input.is_owner,
            input
                .scopes
                .iter()
                .flatten()
                .chain(input.append_scopes.iter().flatten()),
        )
        .map_err(|err| err.to_string())?;
        guard_role_grants(ctx, input.add_role_ids.as_deref()).await?;
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_user_with_access(&access, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateUserStatus` (conduit.resolvers.go:466):
    /// `updateUserStatus(id: ID!, status: UserStatus!): User!` — delegates
    /// to `UserService.UpdateUserStatus`.
    async fn update_user_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: UserStatus,
    ) -> Result<User, String> {
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_user_status_with_access(&access, id.as_str(), status)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteUser` (conduit.resolvers.go:471):
    /// `deleteUser(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_user(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<bool, String> {
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .delete_user_with_access(&access, id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.addUserToProject` (conduit.resolvers.go:541):
    /// `addUserToProject(input: AddUserToProjectInput!): UserProject!` —
    /// delegates to `UserService.AddUserToProject` (biz/user.go:367):
    /// rejects duplicate membership, creates the link with owner/scopes/roles.
    async fn add_user_to_project(
        &self,
        ctx: &Context<'_>,
        input: AddUserToProjectInput,
    ) -> Result<UserProject, String> {
        crate::policy::guard_scope_grant(ctx, input.is_owner, input.scopes.iter().flatten())
            .map_err(|err| err.to_string())?;
        guard_role_grants(ctx, input.role_ids.as_deref()).await?;
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .add_user_to_project_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.removeUserFromProject` (conduit.resolvers.go:547):
    /// `removeUserFromProject(input: RemoveUserFromProjectInput!): Boolean!`
    /// — delegates to `UserService.RemoveUserFromProject` (biz/user.go:408).
    async fn remove_user_from_project(
        &self,
        ctx: &Context<'_>,
        input: RemoveUserFromProjectInput,
    ) -> Result<bool, String> {
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .remove_user_from_project_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateProjectUser` (conduit.resolvers.go:557):
    /// `updateProjectUser(input: UpdateProjectUserInput!): UserProject!` —
    /// delegates to `UserService.UpdateProjectUser` (biz/user.go:447):
    /// partial merge of isOwner / scopes / addRoleIDs / removeRoleIDs.
    async fn update_project_user(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectUserInput,
    ) -> Result<UserProject, String> {
        crate::policy::guard_scope_grant(ctx, input.is_owner, input.scopes.iter().flatten())
            .map_err(|err| err.to_string())?;
        guard_role_grants(ctx, input.add_role_ids.as_deref()).await?;
        let services = user_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_USERS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_project_user_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Role CRUD (RUST-P12-001 S07, role slice). Contract: snapshot
    // `type Mutation` lines 828-831. Semantics and tests live in
    // `crate::role`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createRole` (conduit.resolvers.go:480):
    /// `createRole(input: CreateRoleInput!): Role!` — delegates to
    /// `RoleService.CreateRole` (scope-permission check, level/projectID
    /// consistency, duplicate-name probe, ent create).
    async fn create_role(&self, ctx: &Context<'_>, input: CreateRoleInput) -> Result<Role, String> {
        // P-31: a non-owner may only seed a role with scopes they hold
        // (Go CanGrantRole -> CanGrantScopes).
        crate::policy::guard_scope_grant(ctx, None, input.scopes.iter().flatten())
            .map_err(|err| err.to_string())?;
        let services = role_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_ROLES,
        )
        .map_err(|error| error.to_string())?;
        services
            .create_role_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateRole` (conduit.resolvers.go:485):
    /// `updateRole(id: ID!, input: UpdateRoleInput!): Role!` — delegates
    /// to `RoleService.UpdateRole` (partial merge, scope-set/append/clear
    /// with clear-last precedence).
    async fn update_role(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdateRoleInput,
    ) -> Result<Role, String> {
        // P-31: a non-owner may only grant a role scopes they hold.
        crate::policy::guard_scope_grant(
            ctx,
            None,
            input
                .scopes
                .iter()
                .flatten()
                .chain(input.append_scopes.iter().flatten()),
        )
        .map_err(|err| err.to_string())?;
        let services = role_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_ROLES,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_role_with_access(&access, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deleteRole` (conduit.resolvers.go:490):
    /// `deleteRole(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_role(&self, ctx: &Context<'_>, id: async_graphql::ID) -> Result<bool, String> {
        let services = role_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_ROLES,
        )
        .map_err(|error| error.to_string())?;
        services
            .delete_role_with_access(&access, id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDeleteRoles` (conduit.resolvers.go:500):
    /// `bulkDeleteRoles(ids: [ID!]!): Boolean!` — delegates to
    /// `RoleService.BulkDeleteRoles` (biz/role.go:214): empty ids is a
    /// no-op, otherwise cascade-delete by IDs.
    async fn bulk_delete_roles(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = role_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_ROLES,
        )
        .map_err(|error| error.to_string())?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_delete_roles_with_access(&access, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // Prompt CRUD (RUST-P12-001 S07, prompt slice). Contract: snapshot
    // `extend type Mutation` (prompt.graphql) lines 9271-9277. Semantics and
    // tests live in `crate::prompt`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createPrompt` (prompt.resolvers.go:17):
    /// `createPrompt(input: CreatePromptInput!): Prompt!` — delegates to
    /// `PromptService.CreatePrompt` (validate settings, duplicate-name probe
    /// within project, ent defaults, async prompt cache reload).
    async fn create_prompt(
        &self,
        ctx: &Context<'_>,
        input: CreatePromptInput,
    ) -> Result<Prompt, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .create_prompt_with_access(&access, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updatePrompt` (prompt.resolvers.go:22):
    /// `updatePrompt(id: ID!, input: UpdatePromptInput!): Prompt!` —
    /// delegates to `PromptService.UpdatePrompt` (validate settings when
    /// present, duplicate-name probe excluding self, partial merge).
    async fn update_prompt(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdatePromptInput,
    ) -> Result<Prompt, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_prompt_with_access(&access, id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deletePrompt` (prompt.resolvers.go:27):
    /// `deletePrompt(id: ID!): Boolean!` — returns `false` plus the error on
    /// failure, `true` on success.
    async fn delete_prompt(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .delete_prompt_with_access(&access, id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updatePromptStatus` (prompt.resolvers.go:36):
    /// `updatePromptStatus(id: ID!, status: PromptStatus!): Boolean!` —
    /// delegates to `PromptService.UpdatePromptStatus`.
    async fn update_prompt_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: PromptStatus,
    ) -> Result<bool, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        services
            .update_prompt_status_with_access(&access, id.as_str(), status)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDeletePrompts` (prompt.resolvers.go:46):
    /// `bulkDeletePrompts(ids: [ID!]!): Boolean!` — empty ids is a no-op.
    async fn bulk_delete_prompts(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_delete_prompts_with_access(&access, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkEnablePrompts` (prompt.resolvers.go:57):
    /// `bulkEnablePrompts(ids: [ID!]!): Boolean!` — bulk SetStatus enabled.
    async fn bulk_enable_prompts(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_enable_prompts_with_access(&access, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDisablePrompts` (prompt.resolvers.go:68):
    /// `bulkDisablePrompts(ids: [ID!]!): Boolean!` — bulk SetStatus disabled.
    async fn bulk_disable_prompts(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_mutation_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::WRITE_PROMPTS,
        )
        .map_err(|error| error.to_string())?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_disable_prompts_with_access(&access, id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // PromptProtectionRule CRUD (RUST-P12-001 S07, prompt slice). Contract:
    // snapshot `extend type Mutation` (prompt_protection_rule.graphql) lines
    // 9318-9325. Semantics and tests live in `crate::prompt`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createPromptProtectionRule`
    /// (prompt_protection_rule.resolvers.go:18):
    /// `createPromptProtectionRule(input: CreatePromptProtectionRuleInput!):
    /// PromptProtectionRule!` — delegates to
    /// `PromptProtectionRuleService.CreateRule` (validate settings, duplicate-
    /// name probe, ent create, async cache reload).
    async fn create_prompt_protection_rule(
        &self,
        ctx: &Context<'_>,
        input: CreatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        services
            .create_prompt_protection_rule(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updatePromptProtectionRule`
    /// (prompt_protection_rule.resolvers.go:23):
    /// `updatePromptProtectionRule(id: ID!, input:
    /// UpdatePromptProtectionRuleInput!): PromptProtectionRule!` —
    /// delegates to `PromptProtectionRuleService.UpdateRule` (effective
    /// pattern/settings resolved BEFORE re-validation, partial merge).
    async fn update_prompt_protection_rule(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: UpdatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        services
            .update_prompt_protection_rule(id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.deletePromptProtectionRule`
    /// (prompt_protection_rule.resolvers.go:28):
    /// `deletePromptProtectionRule(id: ID!): Boolean!`.
    async fn delete_prompt_protection_rule(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        services
            .delete_prompt_protection_rule(id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updatePromptProtectionRuleStatus`
    /// (prompt_protection_rule.resolvers.go:37):
    /// `updatePromptProtectionRuleStatus(id: ID!, status:
    /// PromptProtectionRuleStatus!): Boolean!`.
    async fn update_prompt_protection_rule_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: crate::prompt::PromptProtectionRuleStatus,
    ) -> Result<bool, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        services
            .update_prompt_protection_rule_status(id.as_str(), status)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDeletePromptProtectionRules`
    /// (prompt_protection_rule.resolvers.go:46):
    /// `bulkDeletePromptProtectionRules(ids: [ID!]!): Boolean!` — empty ids
    /// is a no-op.
    async fn bulk_delete_prompt_protection_rules(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_delete_prompt_protection_rules(id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkEnablePromptProtectionRules`
    /// (prompt_protection_rule.resolvers.go:55):
    /// `bulkEnablePromptProtectionRules(ids: [ID!]!): Boolean!` — empty ids
    /// is a no-op.
    async fn bulk_enable_prompt_protection_rules(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_enable_prompt_protection_rules(id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDisablePromptProtectionRules`
    /// (prompt_protection_rule.resolvers.go:64):
    /// `bulkDisablePromptProtectionRules(ids: [ID!]!): Boolean!` — empty ids
    /// is a no-op.
    async fn bulk_disable_prompt_protection_rules(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        services
            .bulk_disable_prompt_protection_rules(id_strs)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.previewPromptProtectionRule`
    /// (prompt_protection_rule.resolvers.go:73):
    /// `previewPromptProtectionRule(input: PromptProtectionRulePreviewInput!):
    /// PromptProtectionRulePreviewResult!` — compile regex, match test text,
    /// apply mask/reject semantics.
    async fn preview_prompt_protection_rule(
        &self,
        ctx: &Context<'_>,
        input: PromptProtectionRulePreviewInput,
    ) -> Result<PromptProtectionRulePreviewResult, String> {
        let services = prompt_protection_rule_mutation_services(ctx)?;
        services
            .preview_prompt_protection_rule(input)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Data Storage CRUD (RUST-P12-001 S07, data_storage slice). Contract:
    // snapshot `extend type Mutation` lines 848-849. Semantics and tests
    // live in `crate::data_storage`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.createDataStorage` (conduit.resolvers.go:565):
    /// `createDataStorage(input: CreateDataStorageInput!): DataStorage!` —
    /// delegates to `DataStorageService.CreateDataStorage`
    /// (biz/data_storage.go:192): duplicate-name check → ent create with
    /// `primary=false` + `status=active` defaults → cache invalidation.
    async fn create_data_storage(
        &self,
        ctx: &Context<'_>,
        input: crate::data_storage::CreateDataStorageInput,
    ) -> Result<crate::data_storage::DataStorage, String> {
        let services = crate::data_storage::data_storage_mutation_services(ctx)?;
        services
            .create_data_storage(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.updateDataStorage` (conduit.resolvers.go:570):
    /// `updateDataStorage(id: ID!, input: UpdateDataStorageInput!):
    /// DataStorage!` — delegates to `DataStorageService.UpdateDataStorage`
    /// (biz/data_storage.go:224): read existing, duplicate-name check
    /// excluding self, merge settings (sensitive credential fields kept
    /// when input omits them), ent update.
    async fn update_data_storage(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        input: crate::data_storage::UpdateDataStorageInput,
    ) -> Result<crate::data_storage::DataStorage, String> {
        let services = crate::data_storage::data_storage_mutation_services(ctx)?;
        services
            .update_data_storage(id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // System settings basics (RUST-P12-001 S07, system slice). Contract:
    // snapshot `extend type Mutation` lines 9707 / 9714 / 9718-9719.
    // Semantics and tests live in `crate::system`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.completeOnboarding` (system.resolvers.go:112-120):
    /// mark the system-level onboarding completed. The `input.dummy` field
    /// exists in the GraphQL contract so the input is non-null at the schema
    /// layer; the resolver ignores it (Go parity).
    async fn complete_onboarding(
        &self,
        ctx: &Context<'_>,
        _input: CompleteOnboardingInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        match services.complete_onboarding().await {
            Ok(()) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Mirrors Go `Mutation.updateSecuritySettings` (system.resolvers.go:209-234):
    /// read current settings, apply the partial merge (`None` fields preserve
    /// current), persist, return `true`.
    async fn update_security_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateSecuritySettingsInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let current = services
            .security_settings()
            .await
            .map_err(|err| err.to_string())?;
        let merged = merge_security_settings(&current, &input);
        services
            .set_security_settings(merged)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.saveProxyPreset` (system.resolvers.go:286-294):
    /// upsert the preset keyed by URL.
    async fn save_proxy_preset(
        &self,
        ctx: &Context<'_>,
        input: SaveProxyPresetInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let preset: ProxyPreset = input.into();
        match services.save_proxy_preset(preset).await {
            Ok(()) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Mirrors Go `Mutation.deleteProxyPreset` (system.resolvers.go:296-304):
    /// remove the preset with the given URL.
    async fn delete_proxy_preset(&self, ctx: &Context<'_>, url: String) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        match services.delete_proxy_preset(&url).await {
            Ok(()) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }

    // -----------------------------------------------------------------
    // RUST-P12-001 S07 (continuation) — five additional settings domains.
    // Each mutation mirrors the Go resolver in `system.resolvers.go`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.updateBrandSettings` (system.resolvers.go:23-47):
    /// each `Some` field is forwarded to its dedicated setter; `None` fields
    /// are no-ops.
    async fn update_brand_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateBrandSettingsInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        if let Some(name) = input.brand_name.as_deref() {
            services
                .set_brand_name(name)
                .await
                .map_err(|err| err.to_string())?;
        }
        if let Some(logo) = input.brand_logo.as_deref() {
            services
                .set_brand_logo(logo)
                .await
                .map_err(|err| err.to_string())?;
        }
        if let Some(title) = input.title.as_deref() {
            services
                .set_title(title)
                .await
                .map_err(|err| err.to_string())?;
        }
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateStoragePolicy` (system.resolvers.go:49-57):
    /// forward the typed policy to the service.
    async fn update_storage_policy(
        &self,
        ctx: &Context<'_>,
        input: UpdateStoragePolicyInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let policy: StoragePolicy = input.into();
        services
            .set_storage_policy(policy)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateRetryPolicy` (system.resolvers.go:59-67).
    async fn update_retry_policy(
        &self,
        ctx: &Context<'_>,
        input: UpdateRetryPolicyInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let policy: RetryPolicy = input.into();
        services
            .set_retry_policy(policy)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateUserAgentPassThroughSettings`
    /// (system.resolvers.go:306-309): forward the boolean.
    async fn update_user_agent_pass_through_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateUserAgentPassThroughSettingsInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        services
            .set_user_agent_pass_through(input.enabled)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateDefaultDataStorage`
    /// (system.resolvers.go:102-110): parse the GUID wire form, forward the
    /// numeric id to the service.
    async fn update_default_data_storage(
        &self,
        ctx: &Context<'_>,
        input: UpdateDefaultDataStorageInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let guid = crate::node::parse_guid(input.data_storage_id.as_str())
            .map_err(|err| err.to_string())?;
        services
            .set_default_data_storage_id(guid.id)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateSystemGeneralSettings`
    /// (system.resolvers.go:160-167): forward the settings struct to the
    /// service, then reschedule the backup service (Go-specific; we only
    /// persist here).
    async fn update_system_general_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateSystemGeneralSettingsInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let mut settings = services
            .general_settings()
            .await
            .map_err(|err| err.to_string())?;
        if let Some(value) = input.accounting_currency_code {
            settings.accounting_currency_code = value;
        }
        if let Some(value) = input.timezone {
            settings.timezone = value;
        }
        if let Some(value) = input.credit_display_name {
            settings.credit_display_name = value;
        }
        if let Some(value) = input.credits_per_accounting_unit {
            settings.credits_per_accounting_unit = crate::scalars::DecimalScalar(value.0);
        }
        if let Some(value) = input.exchange_rates {
            settings.exchange_rates = value
                .into_iter()
                .map(|rate| crate::system::CurrencyExchangeRate {
                    currency_code: rate.currency_code,
                    quote_per_accounting_unit: crate::scalars::DecimalScalar(
                        rate.quote_per_accounting_unit.0,
                    ),
                })
                .collect();
        }
        services
            .set_general_settings(
                ctx.data_opt::<crate::me::CurrentUser>()
                    .map(|user| user.user_id),
                settings,
            )
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateSystemModelSettings`
    /// (system.resolvers.go:79-100): when `input.developer_settings` is
    /// `None`, preserve the current developer settings (older clients); then
    /// persist the merged settings.
    async fn update_system_model_settings(
        &self,
        ctx: &Context<'_>,
        input: UpdateSystemModelSettingsInput,
    ) -> Result<bool, String> {
        let services = system_settings_services(ctx)?;
        let mut settings: SystemModelSettings = input.into();
        // Go resolver (system.resolvers.go:86-92): if input.DeveloperSettings
        // == nil, read the current value and preserve it.
        if settings.developer_settings.is_empty() {
            let current = services
                .model_settings()
                .await
                .map_err(|err| err.to_string())?;
            settings.developer_settings = current.developer_settings;
        }
        services
            .set_model_settings(settings)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // GAP-D — system channel-settings + pass-through mutations. Semantics
    // and tests live in `crate::system_ext`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.updateSystemChannelSettings`
    /// (system.resolvers.go:143-158): read current-or-default, merge each
    /// provided sub-object (probe / autoSync) without overwriting the other,
    /// persist, return `true`.
    async fn update_system_channel_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::system_ext::UpdateSystemChannelSettingsInput,
    ) -> Result<bool, String> {
        let services = crate::system_ext::system_channel_services(ctx)?;
        let current = services
            .channel_setting_or_default()
            .await
            .map_err(|err| err.to_string())?;
        let merged = crate::system_ext::merge_channel_settings(current, input);
        services
            .set_channel_setting(merged)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    async fn update_public_channel_health_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_probe_ext::UpdatePublicChannelHealthSettingsInput,
    ) -> Result<bool, String> {
        crate::channel_probe_ext::channel_probe_services(ctx)
            .map_err(|error| error.to_string())?
            .set_public_channel_health_settings(input.enabled)
            .await
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updatePassThroughSettings`
    /// (system.resolvers.go:317-324): forward the boolean to the service.
    async fn update_pass_through_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::system_ext::UpdatePassThroughSettingsInput,
    ) -> Result<bool, String> {
        let services = crate::system_ext::system_channel_services(ctx)?;
        services
            .set_pass_through(input.enabled)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // -----------------------------------------------------------------
    // GAP-A — channel extended + bulk mutations. Types + service trait +
    // tests live in `crate::channel_ext`; the host injects a
    // `ChannelExtMutationServices` implementation.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.updateChannelStatus` (conduit.resolvers.go): set
    /// the channel status and return the updated channel.
    async fn update_channel_status(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
        status: crate::channel::ChannelStatus,
    ) -> Result<Channel, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .update_channel_status(id.as_str(), status)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.duplicateChannel` (conduit.resolvers.go): clone the
    /// source channel, overriding fields from `input`.
    async fn duplicate_channel(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "sourceID")] source_id: async_graphql::ID,
        input: CreateChannelInput,
    ) -> Result<Channel, String> {
        let mut input = input;
        validate_and_normalize_channel_settings_input(input.settings.as_mut())
            .map_err(|err| err.to_string())?;
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        let actor_user_id = ctx
            .data_opt::<crate::me::CurrentUser>()
            .map(|user| user.user_id);
        services
            .duplicate_channel(actor_user_id, source_id.as_str(), input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.saveChannelEndpoints` (conduit.resolvers.go):
    /// replace the channel's endpoint list, return the updated channel.
    async fn save_channel_endpoints(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_ext::SaveChannelEndpointsInput,
    ) -> Result<Channel, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .save_channel_endpoints(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.testChannel` (conduit.resolvers.go): run a
    /// connectivity probe against the channel, return latency/success.
    async fn test_channel(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_ext::TestChannelInput,
    ) -> Result<crate::channel_ext::TestChannelPayload, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .test_channel(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.testChannelAPIKey` (conduit.resolvers.go): probe a
    /// single provided API key against the channel.
    #[graphql(name = "testChannelAPIKey")]
    async fn test_channel_api_key(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        key: String,
        #[graphql(name = "modelID")] model_id: Option<String>,
    ) -> Result<crate::channel_ext::TestApiKeyResult, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .test_channel_api_key(channel_id.as_str(), key.as_str(), model_id)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.testChannelAPIKeys` (conduit.resolvers.go): probe
    /// every configured API key on the channel, return the aggregate.
    #[graphql(name = "testChannelAPIKeys")]
    async fn test_channel_api_keys(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        #[graphql(name = "modelID")] model_id: Option<String>,
    ) -> Result<crate::channel_ext::TestChannelApiKeysPayload, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .test_channel_api_keys(channel_id.as_str(), model_id)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.bulkArchiveChannels` (conduit.resolvers.go): the
    /// resolver returns `false, err` on failure and `true` on success.
    async fn bulk_archive_channels(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .bulk_archive_channels(ids)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDisableChannels` (conduit.resolvers.go).
    async fn bulk_disable_channels(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .bulk_disable_channels(ids)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkEnableChannels` (conduit.resolvers.go).
    async fn bulk_enable_channels(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .bulk_enable_channels(ids)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkRecoverChannels` (conduit.resolvers.go).
    async fn bulk_recover_channels(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .bulk_recover_channels(ids)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.bulkDeleteChannels` (conduit.resolvers.go).
    async fn bulk_delete_channels(
        &self,
        ctx: &Context<'_>,
        ids: Vec<async_graphql::ID>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .bulk_delete_channels(ids)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.syncChannelModels` (conduit.resolvers.go): sync the
    /// channel's supported-model list (optionally filtered by `pattern`),
    /// return the channel id + resulting model list.
    async fn sync_channel_models(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        pattern: Option<String>,
    ) -> Result<crate::channel_ext::SyncChannelModelsPayload, String> {
        let services = crate::channel_ext::channel_ext_mutation_services(ctx)?;
        services
            .sync_channel_models(channel_id.as_str(), pattern)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-A2 — channel bulk-create / import / ordering + per-channel
    // API-key management. Types + service trait + tests live in
    // `crate::channel_ext2`; Go source is `conduit.resolvers.go`.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.bulkCreateChannels` (conduit.resolvers.go:166):
    /// fan out one channel per API key in the input, return all created rows.
    async fn bulk_create_channels(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_ext2::BulkCreateChannelsInput,
    ) -> Result<Vec<Channel>, String> {
        let mut input = input;
        validate_and_normalize_channel_settings_input(input.settings.as_mut())
            .map_err(|err| err.to_string())?;
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .bulk_create_channels(input)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.bulkImportChannels` (conduit.resolvers.go:319):
    /// best-effort import, returning the success/created/failed aggregate.
    async fn bulk_import_channels(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_ext2::BulkImportChannelsInput,
    ) -> Result<crate::channel_ext2::BulkImportChannelsResult, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .bulk_import_channels(input.channels)
            .await
            .map_err(|err| err.to_string())
    }

    /// Mirrors Go `Mutation.bulkUpdateChannelOrdering`
    /// (conduit.resolvers.go:335): persist the new ordering weights and derive
    /// `success = true` + `updated = len(channels)`.
    async fn bulk_update_channel_ordering(
        &self,
        ctx: &Context<'_>,
        input: crate::channel_ext2::BulkUpdateChannelOrderingInput,
    ) -> Result<crate::channel_ext2::BulkUpdateChannelOrderingResult, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        let channels = services
            .bulk_update_channel_ordering(input.channels)
            .await
            .map_err(|err| err.to_string())?;
        Ok(crate::channel_ext2::BulkUpdateChannelOrderingResult {
            success: true,
            updated: channels.len() as i32,
            channels,
        })
    }

    /// Mirrors Go `Mutation.disableChannelAPIKey` (conduit.resolvers.go:349):
    /// returns `false, err` on failure, `true` on success.
    #[graphql(name = "disableChannelAPIKey")]
    async fn disable_channel_api_key(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        key: String,
    ) -> Result<bool, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .disable_channel_api_key(channel_id.as_str(), key.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.enableChannelAPIKey` (conduit.resolvers.go:358).
    #[graphql(name = "enableChannelAPIKey")]
    async fn enable_channel_api_key(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        key: String,
    ) -> Result<bool, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .enable_channel_api_key(channel_id.as_str(), key.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.enableAllChannelAPIKeys`
    /// (conduit.resolvers.go:367).
    #[graphql(name = "enableAllChannelAPIKeys")]
    async fn enable_all_channel_api_keys(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
    ) -> Result<bool, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .enable_all_channel_api_keys(channel_id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.enableSelectedChannelAPIKeys`
    /// (conduit.resolvers.go:376).
    #[graphql(name = "enableSelectedChannelAPIKeys")]
    async fn enable_selected_channel_api_keys(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        keys: Vec<String>,
    ) -> Result<bool, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .enable_selected_channel_api_keys(channel_id.as_str(), keys)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.deleteDisabledChannelAPIKeys`
    /// (conduit.resolvers.go:385): remove the listed disabled keys, return the
    /// success/message payload.
    #[graphql(name = "deleteDisabledChannelAPIKeys")]
    async fn delete_disabled_channel_api_keys(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "channelID")] channel_id: async_graphql::ID,
        keys: Vec<String>,
    ) -> Result<crate::channel_ext2::DeleteDisabledApiKeysPayload, String> {
        let services = crate::channel_ext2::channel_bulk_mutation_services(ctx)?;
        services
            .delete_disabled_channel_api_keys(channel_id.as_str(), keys)
            .await
            .map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // GAP-D (remainder) — video-storage / webhook / auto-backup settings
    // + onboarding-completion mutations. Types + service trait + tests
    // live in `crate::system_settings_ext`; the host injects a
    // `SystemSettingsExtServices` implementation.
    // -----------------------------------------------------------------

    /// Mirrors Go `Mutation.updateVideoStorageSettings`
    /// (system.resolvers.go:173-181): forward the typed settings to the
    /// service (Go then reschedules the video worker; we only persist).
    async fn update_video_storage_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::system_settings_ext::UpdateVideoStorageSettingsInput,
    ) -> Result<bool, String> {
        let services = crate::system_settings_ext::system_settings_ext_services(ctx)?;
        let settings: crate::system_settings_ext::VideoStorageSettings = input.into();
        services
            .set_video_storage_settings(settings)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateWebhookNotifierConfig`
    /// (system.resolvers.go:70-77): forward the config struct to the service.
    async fn update_webhook_notifier_config(
        &self,
        ctx: &Context<'_>,
        input: crate::system_settings_ext::WebhookNotifierConfigInput,
    ) -> Result<bool, String> {
        let services = crate::system_settings_ext::system_settings_ext_services(ctx)?;
        let config: crate::system_settings_ext::WebhookNotifierConfig = input.into();
        services
            .set_webhook_notifier_config(config)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.updateAutoBackupSettings`
    /// (backup.resolvers.go:55-...): read current settings, apply the partial
    /// merge (`None` fields preserve current), persist, return `true`. The Go
    /// resolver also owner-gates the call; the host enforces that.
    async fn update_auto_backup_settings(
        &self,
        ctx: &Context<'_>,
        input: crate::system_settings_ext::UpdateAutoBackupSettingsInput,
    ) -> Result<bool, String> {
        let services = crate::system_settings_ext::system_settings_ext_services(ctx)?;
        let current = services
            .auto_backup_settings()
            .await
            .map_err(|err| err.to_string())?;
        let merged = crate::system_settings_ext::merge_auto_backup_settings(current, &input);
        services
            .set_auto_backup_settings(merged)
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.backup` (backup.resolvers.go:21-33): produce a full
    /// backup archive and return it inline as a base64/text `data` field for the
    /// admin UI to download. Delegates to the injected [`BackupExtServices`]
    /// (P-04); the DB dump + JSON assembly live in `conduit-services`.
    async fn backup(
        &self,
        ctx: &Context<'_>,
        input: crate::backup_ext::BackupOptionsInput,
    ) -> Result<crate::backup_ext::BackupPayload, String> {
        let services = crate::backup_ext::backup_ext_services(ctx)?;
        // The service returns the base64-encoded archive (Go
        // `base64.StdEncoding.EncodeToString`); on success we wrap it in the
        // `BackupPayload` the frontend downloads.
        let data = services
            .run_backup(input)
            .await
            .map_err(|err| err.to_string())?;
        Ok(crate::backup_ext::BackupPayload {
            success: true,
            data: Some(data),
            message: Some("Backup completed successfully".to_string()),
        })
    }

    /// Mirrors Go `Mutation.completeSystemModelSettingOnboarding`
    /// (system.resolvers.go:123-131): mark the system-model-setting onboarding
    /// as complete. The `dummy` input field is ignored (schema placeholder).
    async fn complete_system_model_setting_onboarding(
        &self,
        ctx: &Context<'_>,
        _input: crate::system_settings_ext::CompleteSystemModelSettingOnboardingInput,
    ) -> Result<bool, String> {
        let services = crate::system_settings_ext::system_settings_ext_services(ctx)?;
        services
            .complete_system_model_setting_onboarding()
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// Mirrors Go `Mutation.completeAutoDisableChannelOnboarding`
    /// (system.resolvers.go:133-141): mark the auto-disable-channel onboarding
    /// as complete. The `dummy` input field is ignored (schema placeholder).
    async fn complete_auto_disable_channel_onboarding(
        &self,
        ctx: &Context<'_>,
        _input: crate::system_settings_ext::CompleteAutoDisableChannelOnboardingInput,
    ) -> Result<bool, String> {
        let services = crate::system_settings_ext::system_settings_ext_services(ctx)?;
        services
            .complete_auto_disable_channel_onboarding()
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    // ----- me account slice (GAP-H): updateMe / updateMyPassword /
    // unlinkOIDCIdentity. Bodies pasted from `me_ext.rs` wiring doc; each reads
    // the per-request `CurrentUser` (Go `contexts.GetUser(ctx).ID`) and the
    // host-injected `MeMutationServices`. -----

    /// `Mutation.updateMe(input: UpdateMeInput!): User!` — Go
    /// `me.resolvers.go` `UpdateMe`: read current user from context, then
    /// `UserService.UpdateUser(ctx, userCtx.ID, UpdateUserParams{...})` with
    /// the four optional profile fields (partial merge). Returns the updated
    /// user.
    async fn update_me(
        &self,
        ctx: &Context<'_>,
        input: crate::me_ext::UpdateMeInput,
    ) -> Result<crate::user::User, String> {
        let user_id = crate::me::current_user(ctx)?.user_id;
        let services = crate::me_ext::me_mutation_services(ctx)?;
        services
            .update_me(user_id, input)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Mutation.updateMyPassword(input: UpdateMyPasswordInput!): Boolean!` —
    /// Go `me.resolvers.go` `UpdateMyPassword`: read current user, then
    /// `UserService.UpdatePassword(ctx, userCtx.ID, oldPassword, newPassword)`.
    /// Returns `true` on success.
    async fn update_my_password(
        &self,
        ctx: &Context<'_>,
        input: crate::me_ext::UpdateMyPasswordInput,
    ) -> Result<bool, String> {
        let user_id = crate::me::current_user(ctx)?.user_id;
        let services = crate::me_ext::me_mutation_services(ctx)?;
        services
            .update_my_password(
                user_id,
                input.old_password.unwrap_or_default(),
                input.new_password,
            )
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }

    /// `Mutation.unlinkOIDCIdentity(id: ID!): Boolean!` — Go `me.resolvers.go`
    /// `UnlinkOIDCIdentity`: read current user, verify the identity belongs to
    /// them, guard against unlinking the last OIDC identity of an OIDC-only
    /// account, then delete it. Returns `true` on success. The ownership /
    /// last-identity guards live in the host implementation (they need DB
    /// access); this resolver forwards the current user id + the raw `ID!`.
    #[graphql(name = "unlinkOIDCIdentity")]
    async fn unlink_oidc_identity(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> Result<bool, String> {
        let user_id = crate::me::current_user(ctx)?.user_id;
        let services = crate::me_ext::me_mutation_services(ctx)?;
        services
            .unlink_oidc_identity(user_id, id.to_string())
            .await
            .map_err(|err| err.to_string())?;
        Ok(true)
    }
}

/// Resolves the injected [`QuotaMutationServices`] from the async-graphql
/// context data bag. If no service was wired (e.g. the bare SDL-smoke schema),
/// returns the Go-equivalent "service unavailable" error message so callers
/// surface the familiar failure mode rather than panicking.
fn quota_services(ctx: &Context<'_>) -> Result<Arc<dyn QuotaMutationServices>, String> {
    match ctx.data::<Arc<dyn QuotaMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(QuotaMutationError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, Value};

    use super::*;

    // Mutex-guard helper that never panics on poison (matches the workspace
    // convention in `provider_quota_service.rs`).
    fn locked_count(guard: std::sync::MutexGuard<'_, u32>) -> u32 {
        *guard
    }

    // ---------------------------------------------------------------------
    // In-memory fake service for hermetic resolver-level tests. Mirrors the
    // Go resolver's call sequence without touching any DB / HTTP.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeQuotaServices {
        settings: Arc<Mutex<QuotaEnforcementSettings>>,
        manual_check_calls: Arc<Mutex<u32>>,
        reset_calls: Arc<Mutex<Vec<String>>>,
        reset_error: Option<QuotaMutationError>,
        manual_check_error: Option<QuotaMutationError>,
    }

    #[async_trait::async_trait]
    impl QuotaMutationServices for FakeQuotaServices {
        async fn manual_check(&self) -> Result<(), QuotaMutationError> {
            let mut guard = self
                .manual_check_calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *guard += 1;
            drop(guard);
            match &self.manual_check_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn probe_channel_quota(
            &self,
            _channel_id: &str,
            _new_api_pat: Option<&str>,
            _new_api_user_id: Option<&str>,
        ) -> Result<ChannelQuotaProbeResult, QuotaMutationError> {
            Ok(ChannelQuotaProbeResult {
                success: true,
                adapter: Some("new_api".to_string()),
                message: "probe succeeded".to_string(),
                currency: Some("USD".to_string()),
                total: Some("10".to_string()),
                used: Some("2".to_string()),
                remaining: Some("8".to_string()),
                balance_source: Some("key".to_string()),
                requires_pat: false,
                unlimited: false,
                unlimited_key_count: 0,
                key_count: 1,
                verified: false,
                verified_at: None,
            })
        }

        async fn confirm_channel_quota_probe(
            &self,
            _channel_id: &str,
        ) -> Result<ChannelQuotaProbeResult, QuotaMutationError> {
            let mut result = self.probe_channel_quota(_channel_id, None, None).await?;
            result.verified = true;
            result.verified_at = Some("2026-08-10T00:00:00Z".to_string());
            Ok(result)
        }

        async fn probe_new_api_pricing(
            &self,
            _channel_id: &str,
            _new_api_pat: Option<&str>,
            _new_api_user_id: Option<&str>,
        ) -> Result<NewApiPricingProbeResult, QuotaMutationError> {
            Ok(NewApiPricingProbeResult {
                source: "new_api_pricing".to_string(),
                source_endpoint: "/api/pricing".to_string(),
                fetched_at: "2026-08-13T00:00:00Z".to_string(),
                pricing_version: Some("v1".to_string()),
                account_group: Some("default".to_string()),
                effective_groups: vec!["default".to_string()],
                key_count: 1,
                matched_key_count: 1,
                warnings: Vec::new(),
                models: vec![NewApiModelPricingProbe {
                    model_id: "gpt-test".to_string(),
                    billing_kind: "token".to_string(),
                    quality: "exact".to_string(),
                    group_ratio: Some("1".to_string()),
                    input_per_million: Some("2".to_string()),
                    output_per_million: Some("6".to_string()),
                    cache_read_per_million: None,
                    cache_write_per_million: None,
                    flat_per_request: None,
                    reason: None,
                }],
            })
        }

        async fn reset_channel_quota_now(
            &self,
            channel_id: &str,
        ) -> Result<(), QuotaMutationError> {
            let mut guard = self
                .reset_calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.push(channel_id.to_string());
            drop(guard);
            match &self.reset_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn quota_enforcement_settings(
            &self,
        ) -> Result<QuotaEnforcementSettings, QuotaMutationError> {
            Ok(*self
                .settings
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()))
        }

        async fn set_quota_enforcement_settings(
            &self,
            settings: QuotaEnforcementSettings,
        ) -> Result<(), QuotaMutationError> {
            let mut guard = self
                .settings
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *guard = settings;
            Ok(())
        }
    }

    /// Builds a schema wiring `FakeQuotaServices` into the data bag, plus a
    /// minimal query root so the schema composes.
    type TestSchema = Schema<TestQueryRoot, MutationRoot, EmptySubscription>;

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn ping(&self) -> &'static str {
            "pong"
        }
    }

    fn schema_with_services(services: FakeQuotaServices) -> TestSchema {
        let arc: Arc<dyn QuotaMutationServices> = Arc::new(services);
        Schema::build(TestQueryRoot, MutationRoot, EmptySubscription)
            .data(arc)
            .finish()
    }

    // ---- merge logic --------------------------------------------------

    #[test]
    fn merge_keeps_current_values_when_input_is_none() {
        // Go resolver (system.resolvers.go:190-199): `if input.Enabled != nil`
        // / `if input.Mode != nil` — None fields keep the current value.
        let current = QuotaEnforcementSettings {
            enabled: true,
            mode: QuotaEnforcementMode::DePrioritize,
        };
        let input = UpdateQuotaEnforcementSettingsInput {
            enabled: None,
            mode: None,
        };
        let merged = merge_quota_enforcement_settings(current, &input);
        assert_eq!(
            merged,
            QuotaEnforcementSettings {
                enabled: true,
                mode: QuotaEnforcementMode::DePrioritize,
            }
        );
    }

    #[test]
    fn merge_overrides_only_provided_fields() {
        let current = QuotaEnforcementSettings::default();
        let input = UpdateQuotaEnforcementSettingsInput {
            enabled: Some(true),
            mode: None,
        };
        let merged = merge_quota_enforcement_settings(current, &input);
        assert!(merged.enabled);
        assert_eq!(merged.mode, QuotaEnforcementMode::ExhaustedOnly);
    }

    #[test]
    fn merge_overrides_both_fields_when_both_provided() {
        let current = QuotaEnforcementSettings::default();
        let input = UpdateQuotaEnforcementSettingsInput {
            enabled: Some(true),
            mode: Some(QuotaEnforcementMode::DePrioritize),
        };
        let merged = merge_quota_enforcement_settings(current, &input);
        assert!(merged.enabled);
        assert_eq!(merged.mode, QuotaEnforcementMode::DePrioritize);
    }

    #[test]
    fn default_quota_enforcement_settings_matches_go() {
        // Go system_default.go:76-79: { Enabled: false, Mode: ExhaustedOnly }.
        let default = QuotaEnforcementSettings::default();
        assert!(!default.enabled);
        assert_eq!(default.mode, QuotaEnforcementMode::ExhaustedOnly);
    }

    // ---- resolver wiring (manual_check) -------------------------------

    #[tokio::test]
    async fn check_provider_quotas_returns_true_and_invokes_manual_check() {
        // Go resolver (system.resolvers.go:237-245): ManualCheck then true.
        let fake = FakeQuotaServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema.execute("mutation { checkProviderQuotas }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("checkProviderQuotas", Value::Boolean(true))])
        );
        let count = locked_count(
            fake.manual_check_calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn check_provider_quotas_surfaces_service_unavailable_error() {
        // Go resolver (system.resolvers.go:238-240): nil service -> error.
        let fake = FakeQuotaServices {
            manual_check_error: Some(QuotaMutationError::ServiceUnavailable),
            ..FakeQuotaServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema.execute("mutation { checkProviderQuotas }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("provider quota service is not available"),
            "unexpected error message: {msg}"
        );
    }

    // ---- resolver wiring (reset_channel_quota_now) --------------------

    #[tokio::test]
    async fn reset_channel_quota_now_returns_true_and_forwards_channel_id() {
        // Go resolver (system.resolvers.go:257): ResetChannelQuotaNow then true.
        let fake = FakeQuotaServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(r#"mutation { resetChannelQuotaNow(channelID: "42") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("resetChannelQuotaNow", Value::Boolean(true))])
        );
        let reset_calls = fake
            .reset_calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(reset_calls, vec!["42".to_string()]);
    }

    #[tokio::test]
    async fn reset_channel_quota_now_surfaces_reset_error() {
        let fake = FakeQuotaServices {
            reset_error: Some(QuotaMutationError::Reset("boom".to_string())),
            ..FakeQuotaServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(r#"mutation { resetChannelQuotaNow(channelID: "7") }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to reset channel quota"), "msg: {msg}");
        assert!(msg.contains("boom"), "msg: {msg}");
    }

    // ---- resolver wiring (update_quota_enforcement_settings) ----------

    #[tokio::test]
    async fn update_quota_enforcement_settings_writes_merged_value() {
        // Go resolver (system.resolvers.go:185-207): read default, merge the
        // `enabled: true` override, write back, return true.
        let fake = FakeQuotaServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateQuotaEnforcementSettings(
                        input: { enabled: true, mode: DE_PRIORITIZE }
                    )
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("updateQuotaEnforcementSettings", Value::Boolean(true))])
        );
        let stored = *fake
            .settings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(stored.enabled);
        assert_eq!(stored.mode, QuotaEnforcementMode::DePrioritize);
    }

    #[tokio::test]
    async fn update_quota_enforcement_settings_keeps_unset_fields_from_current() {
        // Only `enabled` is provided; `mode` must keep the current value.
        let fake = FakeQuotaServices {
            settings: Arc::new(Mutex::new(QuotaEnforcementSettings {
                enabled: false,
                mode: QuotaEnforcementMode::DePrioritize,
            })),
            ..FakeQuotaServices::default()
        };
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateQuotaEnforcementSettings(input: { enabled: true })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let stored = *fake
            .settings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(stored.enabled);
        // Mode was unset in the input -> preserved from current.
        assert_eq!(stored.mode, QuotaEnforcementMode::DePrioritize);
    }

    #[tokio::test]
    async fn probe_new_api_pricing_returns_normalized_preview_without_saving() {
        let schema = schema_with_services(FakeQuotaServices::default());
        let resp = schema
            .execute(
                r#"mutation {
                    probeNewApiPricing(channelID: "gid://conduit/Channel/1") {
                        source
                        effectiveGroups
                        keyCount
                        matchedKeyCount
                        models { modelId billingKind quality inputPerMillion outputPerMillion }
                    }
                }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let json = resp.data.into_json().expect("GraphQL JSON");
        assert_eq!(json["probeNewApiPricing"]["source"], "new_api_pricing");
        assert_eq!(json["probeNewApiPricing"]["matchedKeyCount"], 1);
        assert_eq!(
            json["probeNewApiPricing"]["models"][0]["inputPerMillion"],
            "2"
        );
    }

    // ---- SDL shape parity ---------------------------------------------

    /// Builds a schema with the real MutationRoot and asserts the three
    /// mutations + input type appear in the SDL exactly as the Go contract
    /// declares them (`system.graphql:177-180,373-376` /
    /// snapshot line 9517-9520,9713-9716).
    #[test]
    fn sdl_contains_three_quota_mutations_and_input_type() {
        let arc: Arc<dyn QuotaMutationServices> = Arc::new(FakeQuotaServices::default());
        let sdl = Schema::build(TestQueryRoot, MutationRoot, EmptySubscription)
            .data(arc)
            .finish()
            .sdl();

        // Input type + fields (nullable per Go contract).
        assert!(sdl.contains("input UpdateQuotaEnforcementSettingsInput {"));
        assert!(
            sdl.contains("enabled: Boolean"),
            "SDL missing nullable `enabled` field: {sdl}"
        );
        assert!(
            sdl.contains("mode: QuotaEnforcementMode"),
            "SDL missing nullable `mode` field: {sdl}"
        );

        // Three mutations, exact signatures.
        assert!(
            sdl.contains("checkProviderQuotas: Boolean!"),
            "SDL missing checkProviderQuotas: {sdl}"
        );
        assert!(
            sdl.contains("resetChannelQuotaNow(channelID: ID!): Boolean!"),
            "SDL missing resetChannelQuotaNow: {sdl}"
        );
        assert!(
            sdl.contains(
                "updateQuotaEnforcementSettings(input: UpdateQuotaEnforcementSettingsInput!): Boolean!"
            ),
            "SDL missing updateQuotaEnforcementSettings: {sdl}"
        );
        assert!(
            sdl.contains("probeNewApiPricing(channelID: ID!, newApiPAT: String, newApiUserID: ID): NewApiPricingProbeResult!"),
            "SDL missing probeNewApiPricing: {sdl}"
        );
    }

    /// Cross-check: the SDL the resolvers emit must agree with the captured
    /// snapshot at `tests/contracts/admin_graphql_schema.graphql` for the three
    /// quota mutations. This pins the parity to the actual Go contract rather
    /// than a fabricated shape.
    #[test]
    fn sdl_matches_snapshot_for_quota_mutations() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| format!("snapshot read failed: {err}"))?;

        // Snapshot declares the input type with both nullable fields. The
        // snapshot file uses mixed CRLF/LF line terminators, so we assert the
        // field lines individually rather than as a contiguous multi-line
        // substring.
        assert!(snapshot.contains("input UpdateQuotaEnforcementSettingsInput {"));
        assert!(
            snapshot.contains("enabled: Boolean"),
            "snapshot missing nullable `enabled` field"
        );
        assert!(
            snapshot.contains("mode: QuotaEnforcementMode"),
            "snapshot missing nullable `mode` field"
        );

        // Snapshot declares the three mutations exactly.
        assert!(snapshot.contains("checkProviderQuotas: Boolean!"));
        assert!(snapshot.contains("resetChannelQuotaNow(channelID: ID!): Boolean!"));
        assert!(snapshot.contains(
            "updateQuotaEnforcementSettings(input: UpdateQuotaEnforcementSettingsInput!): Boolean!"
        ));
        Ok(())
    }

    // ---- helpers ------------------------------------------------------

    /// Builds an expected `Response.data` `Value::Object` from field/value
    /// pairs. The pairs are inserted in order; async-graphql preserves
    /// insertion order in its `Value::Object` (backed by an ordered map), so
    /// callers pass fields in the order the GraphQL query lists them.
    fn data_object<const N: usize>(fields: [(&'static str, Value); N]) -> Value {
        let mut map = async_graphql::indexmap::IndexMap::new();
        for (name, value) in fields {
            map.insert(Name::new(name), value);
        }
        Value::Object(map)
    }
}

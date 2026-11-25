//! P-01 / P-32 — centralized admin-GraphQL authorization.
//!
//! ## Why an extension (not per-resolver guards)
//!
//! The admin schema has ~100 mutation + ~70 query root fields, all pasted into a
//! single `#[Object] impl QueryRoot` (`lib.rs`) and a single
//! `#[Object] impl MutationRoot` (`mutation.rs`). Editing every resolver to call
//! `authorize_current` would be error-prone and would break the ~200 existing
//! tests that build bare schemas via `admin_schema_builder()` without a
//! principal.
//!
//! Instead we enforce authorization in ONE place: an async-graphql
//! [`Extension`] whose [`resolve`](Extension::resolve) hook runs for every field
//! and, for ROOT fields only (`parent_type` is `QueryRoot`/`MutationRoot`, not
//! introspection), looks up the field's required authorization in [`field_authz`]
//! and checks it against the per-request principal published into the data bag by
//! the HTTP layer (`graphql_handlers.rs` → `request.data(auth.into_context())`).
//!
//! The extension is registered **only on the production schema** (in the
//! binary's `wiring.rs`), so the crate's own tests — which build bare schemas —
//! are unaffected. Dedicated tests below build a schema WITH the extension.
//!
//! ## Model vs Go
//!
//! This mirrors Go's *effective* admin authorization, which is a mix of:
//!   - **ent entity `Policy()`** (default-deny, owner short-circuit) for every
//!     entity CRUD/connection field — the scope is the entity's read/write slug
//!     (`conduit/internal/ent/schema/*.go`). System settings go through the
//!     `System` entity (`setSystemValue`/`getSystemValue`), so they gate on
//!     `read_settings`/`write_settings`.
//!   - **resolver `authz.WithScopeDecision`/`RequireScope` stamps** for the
//!     dashboard (`read_dashboard`), `fetchModels` (`write_channels`),
//!     `apiKeyTokenUsageStats` (`read_api_keys`), `resetChannelQuotaNow`
//!     (`write_channels`).
//!   - **`WithSystemBypass` / no gate** for self-service + a few system reads —
//!     any authenticated admin (`me`, `systemStatus`, `brandSettings`, …).
//!   - **explicit `user.IsOwner`** for `updateAutoBackupSettings` → [`OwnerOnly`].
//!
//! The field→authz table was produced by a Go-source audit (each non-obvious
//! entry cites its Go evidence). An owner bypasses every [`Scope`]; a request
//! with no principal is denied; an unmapped field is denied (fail closed).
//!
//! [`OwnerOnly`]: FieldAuthz::OwnerOnly
//! [`Scope`]: FieldAuthz::Scope

use async_graphql::extensions::{
    Extension, ExtensionContext, ExtensionFactory, NextResolve, ResolveInfo,
};
use async_graphql::{ServerError, ServerResult, Value};
use conduit_auth::request_context::RequestContext;
use conduit_auth::scopes::{slug, supports_project_role};
use std::sync::Arc;

use crate::policy::{authorize_project_resolver, authorize_resolver};
use crate::product_experience::{ProductExperienceServices, ProductMode};

#[async_trait::async_trait]
pub trait SettingsWritePermit: Send {
    async fn release(self: Box<Self>);
}

#[async_trait::async_trait]
pub trait SettingsWriteLock: Send + Sync {
    async fn acquire(&self) -> Result<Box<dyn SettingsWritePermit>, String>;
}

/// The authorization requirement for one root field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAuthz {
    /// No authentication required (Rust-only trivial fields: `health`, `version`).
    Public,
    /// Any authenticated principal (self-service + `WithSystemBypass` reads).
    Authenticated,
    /// Requires the given system scope (owner bypasses).
    Scope(&'static str),
    /// Requires the caller to be an owner (Go `user.IsOwner`, not a scope).
    OwnerOnly,
    /// Unmapped / unknown field — denied (fail closed).
    Deny,
}

/// Central field → authorization table. Keys are the **exact camelCase GraphQL
/// field names** async-graphql exposes (note the mixed `API`/`Api`/`OIDC`/`ID`
/// casing the resolvers pin via `#[graphql(name = ...)]`).
///
/// Cross-entity scope reuse mirrors the Go ent policies: **models**,
/// **channel-model-prices** and **prompt-protection rules** gate on the
/// *channel* scopes; **api-key profile templates** on the *api-key* scopes;
/// **project-user** edits on the *user* scopes; **system settings** on the
/// *settings* scopes (they persist through the `System` entity).
pub fn field_authz(field: &str) -> FieldAuthz {
    use FieldAuthz::{Authenticated, Deny, OwnerOnly, Public, Scope};
    match field {
        // ── Rust-only trivial ─────────────────────────────────────────────
        "health" | "version" => Public,
        "changeSetByOperation" | "governanceSettings" | "governanceCapabilities" | "governedChangeBase" | "changeSets" | "changeSet" | "changeSetItems" | "changeSetItem" | "changeSetEvents" => Authenticated,
        "setProjectMemberSpendLimit" => Scope(slug::WRITE_BILLING),
        "updateGovernanceSettings" => Scope(slug::MANAGE_GOVERNANCE),
        "claimGovernedChangeSecrets" | "proposeGovernedChange" | "validateChangeSet" => Authenticated,
        "reviewChangeSet" => Authenticated,
        "publishChangeSet" => Authenticated,
        "emergencyRestrictAccess" => Scope(slug::EMERGENCY_REVOKE),
        "recoverGovernancePolicy" => OwnerOnly,
        "enumCasingProbe" | "connectionProbe" => Authenticated,

        // ── self-service / WithSystemBypass reads (any authenticated admin) ─
        "me" | "myProjects" | "updateMe" | "updateMyPassword" | "unlinkOIDCIdentity"
        | "systemStatus" | "brandSettings" | "onboardingInfo" | "systemModelSettings"
        | "systemVersion" | "checkForUpdate" | "allScopes" | "systemGeneralSettings"
        | "productExperienceSettings" => Authenticated,

        // Relay node/nodes: Go gates per resolved entity via ent policy; the
        // field layer only requires auth (see the residual-hole note in 问题.md).
        "node" | "nodes" => Authenticated,

        // pure regex preview — no DB, no scope in Go.
        "previewPromptProtectionRule" => Authenticated,

        // quota manual actions — Go has no scope stamp (system.resolvers.go:237).
        "checkProviderQuotas" | "manualCheck" => Authenticated,

        // The Go resolvers carry no scope stamp, but BackupService.Backup and
        // Restore explicitly reject non-owner users. Enforce that boundary at
        // GraphQL entry instead of exposing database secrets to every login.
        "backup" | "restore" => OwnerOnly,

        // ── channels (write) ──────────────────────────────────────────────
        "initializeNewApiChannel" | "retryNewApiChannelDiscovery" | "createChannel" | "updateChannel" | "deleteChannel" | "updateChannelStatus"
        | "duplicateChannel" | "saveChannelEndpoints" | "bulkArchiveChannels"
        | "bulkDisableChannels" | "bulkEnableChannels" | "bulkRecoverChannels"
        | "bulkDeleteChannels" | "bulkCreateChannels" | "bulkImportChannels"
        | "bulkUpdateChannelOrdering" | "syncChannelModels" | "disableChannelAPIKey"
        | "enableChannelAPIKey" | "enableAllChannelAPIKeys" | "enableSelectedChannelAPIKeys"
        | "deleteDisabledChannelAPIKeys"
        | "createChannelOverrideTemplate" | "updateChannelOverrideTemplate"
        | "deleteChannelOverrideTemplate" | "applyChannelOverrideTemplate"
        | "clearChannelOverrideTemplates"
        | "upsertModelRoute" | "createPublicModelWithRoutes" | "applyChannelModelMappings"
        | "setChannelModelMappingAutomation"
        // models + prices + prompt-protection reuse the CHANNEL write scope.
        | "createModel" | "updateModel" | "deleteModel" | "updateModelStatus"
        | "bulkCreateModels" | "bulkArchiveModels" | "bulkDisableModels" | "bulkEnableModels"
        | "bulkDeleteModels"
        | "createPromptProtectionRule" | "updatePromptProtectionRule"
        | "deletePromptProtectionRule" | "updatePromptProtectionRuleStatus"
        | "bulkDeletePromptProtectionRules" | "bulkEnablePromptProtectionRules"
        | "bulkDisablePromptProtectionRules"
        // Go stamps ScopeWriteChannels on these two (resolver / fetch).
        | "resetChannelQuotaNow" | "probeChannelQuota" | "confirmChannelQuotaProbe"
        | "probeNewApiPricing"
        | "fetchModels" => Scope(slug::WRITE_CHANNELS),

        // ── channels (read) ───────────────────────────────────────────────
        // testChannel* are mutations but Go's only gate is the Channel *Query*
        // policy (Channel.Get; no entity write), so read_channels is faithful.
        "testChannel" | "testChannelAPIKey" | "testChannelAPIKeys" | "channelDiscoveryOperation" | "channels" | "models"
        | "promptProtectionRules" | "allChannelSummarys" | "allChannelTags"
        | "countChannelsByType" | "queryChannels" | "queryModels"
        | "queryModelChannelConnections" | "queryUnassociatedChannels" => {
            Scope(slug::READ_CHANNELS)
        }
        "channelOverrideTemplates"
        | "channelProbeData"
        | "upstreamModelDeployments"
        | "modelRoutes" | "previewChannelModelMappings"
        | "channelModelMappingAutomationSettings" => {
            Scope(slug::READ_CHANNELS)
        }

        // ── api keys (write) ──────────────────────────────────────────────
        "createAPIKey" | "updateAPIKey" | "updateAPIKeyStatus" | "rotateAPIKey"
        | "updateAPIKeyProfiles" | "bulkDisableAPIKeys" | "bulkEnableAPIKeys"
        | "bulkArchiveAPIKeys" | "createApiKeyProfileTemplate" | "updateApiKeyProfileTemplate"
        | "deleteApiKeyProfileTemplate" | "loadApiKeyProfileTemplate" => {
            Scope(slug::WRITE_API_KEYS)
        }

        // ── api keys (read) ───────────────────────────────────────────────
        "apiKeys"
        | "apiKeyAssignableGroups"
        | "apiKeyProfileTemplates"
        | "apiKeyQuotaUsages"
        | "apiKeyTokenUsageStats" => {
            Scope(slug::READ_API_KEYS)
        }

        // ── projects ──────────────────────────────────────────────────────
        "createProject" | "updateProject" | "updateProjectStatus" | "updateProjectProfiles"
        | "deleteProject" => Scope(slug::WRITE_PROJECTS),
        "projects" => Scope(slug::READ_PROJECTS),

        // ── users + project-membership (UserProject reuses USER scope) ─────
        "createUser" | "updateUser" | "updateUserStatus" | "deleteUser" | "addUserToProject"
        | "removeUserFromProject" | "updateProjectUser" => Scope(slug::WRITE_USERS),
        "users" => Scope(slug::READ_USERS),

        // ── groups ─────────────────────────────────────────────────────────
        "previewNewApiBillingExpression" => Scope(slug::READ_COMMERCIALIZATION),
        "modelGroups" | "userGroups" | "defaultModelGroupDiagnostic" => Scope(slug::READ_USERS),
        "saveModelGroup" | "saveUserGroup" | "moveUserToGroup" => Scope(slug::WRITE_USERS),
        "apiKeyAssignableModelGroups" | "apiKeyProfileCatalog" => Scope(slug::READ_API_KEYS),
        "apiKeyCreationCatalog" | "assignApiKeyModelGroup" => Scope(slug::WRITE_API_KEYS),
        "createSimpleGroup" | "updateSimpleGroup" | "assignSimpleGroupUsers"
        | "updateSimpleGroupModels" | "updateSimpleGroupPrice" | "deleteSimpleGroup" => {
            Scope(slug::WRITE_GROUPS)
        }
        "simpleGroups" => Scope(slug::READ_GROUPS),

        "publicChannelHealth" | "myModelCatalog" => Authenticated,
        "publicChannelHealthSettings" => Scope(slug::READ_SETTINGS),
        "updatePublicChannelHealthSettings" => Scope(slug::WRITE_SETTINGS),

        // ── billing + subscriptions ────────────────────────────────────────
        // Resolvers additionally require GLOBAL billing authority; a Project
        // role accepted by the generic Scope gate cannot access these APIs.
        "operatorPaymentOrders" | "operatorPaymentOrder" => Scope(slug::READ_BILLING),
        "operatorRecheckPaymentOrder" => Scope(slug::WRITE_BILLING),
        "userBalance"
        | "projectBalance"
        | "projectWalletComparison"
        | "creditRedemptionCodes" => {
            Scope(slug::READ_BILLING)
        }
        "userSubscriptions" | "subscriptionProjects" | "subscriptionPlans" => {
            Scope(slug::READ_SUBSCRIPTIONS)
        }
        "myBalance" | "mySubscriptions" | "myProjectBalance"
        | "myProjectWalletComparison" | "myPrimaryProject" | "myProjectFunds" => Authenticated,
        "projectMemberSpendLimit" => Scope(slug::READ_BILLING),
        "grantUserCredit"
        | "grantProjectCredit"
        | "createCreditRedemptionCodes"
        | "revokeCreditRedemptionCode" => Scope(slug::GRANT_CREDIT),
        "redeemCreditCode" | "paymentCapabilities" | "paymentOrders" | "paymentOrder" | "createPaymentOrder" | "recheckPaymentOrder" => Authenticated,
        "createSubscriptionPlan" | "updateSubscriptionPlan" | "assignUserSubscription"
        | "refreshSubscriptionAllowance" | "pauseUserSubscription"
        | "resumeUserSubscription" | "cancelUserSubscription" | "renewUserSubscription"
        | "setSubscriptionAutoRenew" => Scope(slug::WRITE_SUBSCRIPTIONS),

        // ── roles ─────────────────────────────────────────────────────────
        "createRole" | "updateRole" | "deleteRole" | "bulkDeleteRoles" => Scope(slug::WRITE_ROLES),
        "roles" => Scope(slug::READ_ROLES),

        // ── prompts ───────────────────────────────────────────────────────
        "createPrompt" | "updatePrompt" | "deletePrompt" | "updatePromptStatus"
        | "bulkDeletePrompts" | "bulkEnablePrompts" | "bulkDisablePrompts" => {
            Scope(slug::WRITE_PROMPTS)
        }
        "prompts" => Scope(slug::READ_PROMPTS),

        // ── data storage ──────────────────────────────────────────────────
        "createDataStorage" | "updateDataStorage" => Scope(slug::WRITE_DATA_STORAGES),
        "dataStorages" => Scope(slug::READ_DATA_STORAGES),

        // ── request-domain reads (Request/Thread/Trace/UsageLog policies) ──
        "requests" | "usageLogs" | "threads" | "traces" | "requestRouteExplanation" => {
            Scope(slug::READ_REQUESTS)
        }

        // ── system settings (persist/read through the System entity) ──────
        "updateBrandSettings" | "updateStoragePolicy" | "updateRetryPolicy"
        | "updateWebhookNotifierConfig" | "updateSystemModelSettings"
        | "updateSystemChannelSettings" | "updateSystemGeneralSettings"
        | "updateDefaultDataStorage" | "updateSecuritySettings"
        | "updateQuotaEnforcementSettings" | "setQuotaEnforcementSettings"
        | "updateUserAgentPassThroughSettings" | "updatePassThroughSettings"
        | "updateVideoStorageSettings" | "saveProxyPreset" | "deleteProxyPreset"
        | "completeOnboarding" | "completeSystemModelSettingOnboarding"
        | "completeAutoDisableChannelOnboarding" => Scope(slug::WRITE_SETTINGS),
        "triggerGcCleanup" => Scope(slug::WRITE_SETTINGS),

        // ── commercialization ──────────────────────────────────────────────
        "createPriceBook" | "saveProviderPriceChangeSet" | "createProviderPriceChangeSet" | "createRetailPriceChangeSet"
        | "saveChannelModelPrices" | "saveRetailPriceChangeSetItem" | "saveRetailPriceFormula"
        | "submitChangeSet" | "approveChangeSet" | "rejectChangeSet" => {
            Scope(slug::WRITE_COMMERCIALIZATION)
        }

        "storagePolicy" | "retryPolicy" | "webhookNotifierConfig" | "systemChannelSettings"
        | "videoStorageSettings" | "quotaEnforcementSettings"
        | "securitySettings" | "proxyPresets" | "userAgentPassThroughSettings"
        | "passThroughSettings" | "defaultDataStorageID" | "autoBackupSettings" => {
            Scope(slug::READ_SETTINGS)
        }
        "previewGcCleanup" => Scope(slug::READ_SETTINGS),
        "priceBooks" | "previewCustomerCharge" | "providerPriceChangeSetOperation" | "providerPriceEditBase" | "retailPriceEditBase"
        | "retailPriceFormulas" | "previewRetailPriceFormula" => {
            Scope(slug::READ_COMMERCIALIZATION)
        }

        "clearCache" | "getCacheDiagnostics" | "updateProductExperienceSettings"
        | "completeFinancialSetupOnboarding" => OwnerOnly,

        // owner-only in Go (backup.resolvers.go:57 checks user.IsOwner).
        "updateAutoBackupSettings" | "triggerAutoBackup" => OwnerOnly,

        // ── dashboard (read_dashboard resolver stamp) ─────────────────────
        "dashboardOverview" | "requestStats" | "tokenStats" | "requestStatsByChannel"
        | "requestStatsByModel" | "requestStatsByAPIKey" | "tokenStatsByAPIKey"
        | "tokenStatsByChannel" | "tokenStatsByModel" | "costStatsByChannel"
        | "costStatsByModel" | "costStatsByAPIKey" | "dailyRequestStats" | "hourlyRequestStats"
        | "topRequestsProjects" | "channelSuccessRates" | "fastestChannels" | "fastestModels"
        | "modelPerformanceStats" | "channelPerformanceStats" => Scope(slug::READ_DASHBOARD),
        "operationsLedger" | "operationsFlow" | "operationsModelSeries" | "providerObservationHistory" => {
            Scope(slug::READ_DASHBOARD)
        }

        // Unmapped → fail closed (a new resolver must be classified first).
        _ => Deny,
    }
}

/// The authorization extension. Stateless — one instance is shared across all
/// requests; per-request identity comes from [`ExtensionContext`].
struct ScopeAuthExtension;

#[async_trait::async_trait]
impl Extension for ScopeAuthExtension {
    async fn resolve(
        &self,
        ctx: &ExtensionContext<'_>,
        info: ResolveInfo<'_>,
        next: NextResolve<'_>,
    ) -> ServerResult<Option<Value>> {
        // Only gate ROOT operation fields; nested field resolution and
        // introspection (`__schema`, `__type`, …) pass through.
        let is_root = matches!(info.parent_type, "QueryRoot" | "MutationRoot");
        if is_root
            && !info.is_for_introspection
            && let Err(message) = enforce_field(ctx, info.name)
        {
            return Err(ServerError::new(message, None));
        }
        if is_root && !info.is_for_introspection && enterprise_only_field(info.name) {
            enforce_product_mode(ctx, info.name).await?;
        }
        let feature = match info.name {
            "operationsLedger" | "operationsFlow" | "operationsModelSeries" => {
                Some("operations_reports")
            }
            "createCreditRedemptionCodes" => Some("redemption_issuance"),
            "createSubscriptionPlan"
            | "updateSubscriptionPlan"
            | "assignUserSubscription"
            | "refreshSubscriptionAllowance"
            | "pauseUserSubscription"
            | "resumeUserSubscription"
            | "cancelUserSubscription"
            | "renewUserSubscription"
            | "setSubscriptionAutoRenew" => Some("subscription_management"),
            _ => None,
        };
        if info.parent_type == "MutationRoot"
            && !info.is_for_introspection
            && let Some(service) = ctx.data_opt::<Arc<dyn crate::governance::GovernanceServices>>()
        {
            let actor = ctx
                .data_opt::<crate::me::CurrentUser>()
                .map(|u| u.user_id)
                .ok_or_else(|| ServerError::new("authentication required", None))?;
            if let Err(message) = service.check_direct(actor, info.name).await {
                let code = if message.starts_with("APPROVAL_REQUIRED") {
                    "APPROVAL_REQUIRED"
                } else {
                    "GOVERNANCE_UNSUPPORTED"
                };
                let mut error = ServerError::new(message, None);
                let mut ext = async_graphql::ErrorExtensionValues::default();
                ext.set("code", code);
                error.extensions = Some(ext);
                return Err(error);
            }
        }
        if is_root
            && let Some(feature) = feature
            && let Some(service) =
                ctx.data_opt::<Arc<dyn crate::product_experience::ProductExperienceServices>>()
        {
            let settings = service
                .settings()
                .await
                .map_err(|_| ServerError::new("feature settings unavailable", None))?;
            if settings
                .features
                .iter()
                .any(|item| item.id == feature && !item.enabled)
            {
                let mut error = ServerError::new("FEATURE_DISABLED", None);
                let mut extensions = async_graphql::ErrorExtensionValues::default();
                extensions.set("code", "FEATURE_DISABLED");
                extensions.set("feature", feature);
                error.extensions = Some(extensions);
                return Err(error);
            }
        }
        let writes_settings = info.parent_type == "MutationRoot"
            && (field_authz(info.name) == FieldAuthz::Scope(slug::WRITE_SETTINGS)
                || matches!(
                    info.name,
                    "updateAutoBackupSettings"
                        | "updateProductExperienceSettings"
                        | "completeFinancialSetupOnboarding"
                        | "setChannelModelMappingAutomation"
                ));
        if writes_settings && let Some(lock) = ctx.data_opt::<Arc<dyn SettingsWriteLock>>() {
            let permit = lock
                .acquire()
                .await
                .map_err(|error| ServerError::new(error, None))?;
            let result = next.run(ctx, info).await;
            permit.release().await;
            return result;
        }
        next.run(ctx, info).await
    }
}

/// Authorize a single root field. Returns the public error message on denial.
fn enforce_field(ctx: &ExtensionContext<'_>, field: &str) -> Result<(), String> {
    let request_context = ctx.data_opt::<RequestContext>();
    let principal = request_context.and_then(|rc| rc.principal.as_ref());
    match field_authz(field) {
        FieldAuthz::Public => Ok(()),
        FieldAuthz::Authenticated => {
            let Some(principal) = principal else {
                return Err(NO_PRINCIPAL.to_string());
            };
            let package_read = matches!(
                field,
                "changeSetByOperation"
                    | "changeSets"
                    | "changeSet"
                    | "changeSetItems"
                    | "changeSetItem"
                    | "changeSetEvents"
            );
            let package_write = matches!(
                field,
                "governedChangeBase"
                    | "proposeGovernedChange"
                    | "validateChangeSet"
                    | "reviewChangeSet"
                    | "publishChangeSet"
            );
            if package_read || package_write {
                // Domain authorization is checked again for the actual package;
                // an unrelated login must not enter the package service at all.
                let read_scopes = [
                    slug::READ_COMMERCIALIZATION,
                    slug::READ_CHANNELS,
                    slug::READ_GROUPS,
                    slug::READ_USERS,
                    slug::READ_API_KEYS,
                    slug::READ_PROJECTS,
                    slug::READ_SETTINGS,
                    slug::READ_BILLING,
                    slug::READ_ROLES,
                    slug::READ_SUBSCRIPTIONS,
                    slug::READ_DATA_STORAGES,
                ];
                let write_scopes = [
                    slug::WRITE_COMMERCIALIZATION,
                    slug::WRITE_CHANNELS,
                    slug::WRITE_GROUPS,
                    slug::WRITE_USERS,
                    slug::WRITE_ROLES,
                    slug::WRITE_API_KEYS,
                    slug::WRITE_PROJECTS,
                    slug::WRITE_SETTINGS,
                    slug::WRITE_BILLING,
                    slug::WRITE_SUBSCRIPTIONS,
                    slug::WRITE_DATA_STORAGES,
                    slug::GRANT_CREDIT,
                ];
                let allowed = write_scopes
                    .into_iter()
                    .chain(read_scopes.into_iter().filter(|_| package_read))
                    .any(|scope| {
                        if supports_project_role(scope)
                            && let Some(project) =
                                request_context.and_then(|rc| rc.project_id.as_deref())
                        {
                            authorize_project_resolver(principal, project, scope).is_ok()
                        } else {
                            authorize_resolver(principal, scope).is_ok()
                        }
                    });
                if !allowed {
                    return Err("authz: required scope: one package domain permission".into());
                }
            }
            Ok(())
        }
        FieldAuthz::Scope(scope) => {
            let Some(principal) = principal else {
                return Err(NO_PRINCIPAL.to_string());
            };
            if matches!(
                field,
                "operatorPaymentOrders"
                    | "operatorPaymentOrder"
                    | "operatorRecheckPaymentOrder"
                    | "modelGroups"
                    | "userGroups"
                    | "saveModelGroup"
                    | "saveUserGroup"
                    | "moveUserToGroup"
            ) {
                authorize_resolver(principal, scope).map_err(|err| err.to_string())
            } else if supports_project_role(scope)
                && let Some(project_id) = request_context.and_then(|rc| rc.project_id.as_deref())
            {
                authorize_project_resolver(principal, project_id, scope)
                    .map_err(|err| err.to_string())
            } else {
                authorize_resolver(principal, scope).map_err(|err| err.to_string())
            }
        }
        FieldAuthz::OwnerOnly => match principal {
            Some(p) if p.is_owner => Ok(()),
            Some(_) => Err("authz: this operation requires owner".to_string()),
            None => Err(NO_PRINCIPAL.to_string()),
        },
        FieldAuthz::Deny => Err(format!("authz: field `{field}` is not authorized")),
    }
}

const NO_PRINCIPAL: &str = "authz: request carries no authenticated principal";

/// GQ-13: root fields that only belong to enterprise mode. In SIMPLE mode the
/// UI hides these pages, but without a server check anyone could call them
/// directly. Fields that simple-mode pages or system settings still use
/// (e.g. `dataStorages` for the storage settings tab) are deliberately left
/// out; only roles, prompts, prompt protection, traces/threads and data
/// storage writes are blocked.
pub fn enterprise_only_field(field: &str) -> bool {
    matches!(
        field,
        "roles"
            | "createRole"
            | "updateRole"
            | "deleteRole"
            | "bulkDeleteRoles"
            | "prompts"
            | "createPrompt"
            | "updatePrompt"
            | "deletePrompt"
            | "updatePromptStatus"
            | "bulkDeletePrompts"
            | "bulkEnablePrompts"
            | "bulkDisablePrompts"
            | "promptProtectionRules"
            | "previewPromptProtectionRule"
            | "createPromptProtectionRule"
            | "updatePromptProtectionRule"
            | "deletePromptProtectionRule"
            | "updatePromptProtectionRuleStatus"
            | "bulkDeletePromptProtectionRules"
            | "bulkEnablePromptProtectionRules"
            | "bulkDisablePromptProtectionRules"
            | "threads"
            | "traces"
            | "createDataStorage"
            | "updateDataStorage"
    )
}

/// Deny an enterprise-only field when the site runs in SIMPLE mode. The site
/// owner (and system/test principals) keep access, because the owner is the
/// one who can switch the mode back and must not lock themselves out. When no
/// product-experience service is wired (crate tests), nothing is enforced.
async fn enforce_product_mode(ctx: &ExtensionContext<'_>, field: &str) -> ServerResult<()> {
    let principal = ctx
        .data_opt::<RequestContext>()
        .and_then(|rc| rc.principal.as_ref());
    if principal.is_some_and(|p| {
        p.is_owner
            || matches!(
                p.kind,
                conduit_auth::PrincipalKind::System | conduit_auth::PrincipalKind::Test
            )
    }) {
        return Ok(());
    }
    let Some(services) = ctx.data_opt::<Arc<dyn ProductExperienceServices>>() else {
        return Ok(());
    };
    let mode = services
        .settings()
        .await
        .map(|settings| settings.mode)
        .map_err(|error| ServerError::new(error.to_string(), None))?;
    if mode == ProductMode::Simple {
        return Err(ServerError::new(
            format!("authz: field `{field}` is only available in enterprise mode"),
            None,
        ));
    }
    Ok(())
}

/// Factory registered on the production schema (`wiring.rs`).
pub struct ScopeAuthExtensionFactory;

impl ExtensionFactory for ScopeAuthExtensionFactory {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(ScopeAuthExtension)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Request, Schema};
    use conduit_auth::Principal;

    // ---- pure table --------------------------------------------------------

    /// GQ-13: every enterprise-only field must be a real, classified root
    /// field (a typo would silently leave the field open in SIMPLE mode), and
    /// the fields simple-mode pages rely on must stay open.
    #[test]
    fn enterprise_only_fields_are_classified_and_simple_fields_stay_open() {
        for field in [
            "roles",
            "prompts",
            "promptProtectionRules",
            "threads",
            "traces",
            "createDataStorage",
            "updateDataStorage",
        ] {
            assert!(enterprise_only_field(field), "{field}");
            assert_ne!(field_authz(field), FieldAuthz::Deny, "{field}");
        }
        for field in [
            "apiKeys",
            "requests",
            "usageLogs",
            "dataStorages",
            "me",
            "channels",
        ] {
            assert!(!enterprise_only_field(field), "{field}");
        }
    }

    #[test]
    fn table_maps_entities_to_the_right_scopes() {
        for field in [
            "initializeNewApiChannel",
            "retryNewApiChannelDiscovery",
            "createChannel",
        ] {
            assert_eq!(field_authz(field), FieldAuthz::Scope(slug::WRITE_CHANNELS));
        }
        // models + prompt-protection reuse the CHANNEL scope (Go parity).
        assert_eq!(
            field_authz("createModel"),
            FieldAuthz::Scope(slug::WRITE_CHANNELS)
        );
        // fetchModels is a query but Go stamps WRITE_CHANNELS.
        assert_eq!(
            field_authz("fetchModels"),
            FieldAuthz::Scope(slug::WRITE_CHANNELS)
        );
        // api-key mutations use the uppercase-API GraphQL name.
        assert_eq!(
            field_authz("createAPIKey"),
            FieldAuthz::Scope(slug::WRITE_API_KEYS)
        );
        assert_eq!(
            field_authz("updateUser"),
            FieldAuthz::Scope(slug::WRITE_USERS)
        );
        // project-user edits gate on the USER scope.
        assert_eq!(
            field_authz("updateProjectUser"),
            FieldAuthz::Scope(slug::WRITE_USERS)
        );
        // settings persist through the System entity → settings scopes.
        assert_eq!(
            field_authz("updateRetryPolicy"),
            FieldAuthz::Scope(slug::WRITE_SETTINGS)
        );
        assert_eq!(
            field_authz("storagePolicy"),
            FieldAuthz::Scope(slug::READ_SETTINGS)
        );
        // dashboard.
        assert_eq!(
            field_authz("dashboardOverview"),
            FieldAuthz::Scope(slug::READ_DASHBOARD)
        );
        // the one dashboard field that differs.
        assert_eq!(
            field_authz("apiKeyTokenUsageStats"),
            FieldAuthz::Scope(slug::READ_API_KEYS)
        );
        assert_eq!(
            field_authz("apiKeys"),
            FieldAuthz::Scope(slug::READ_API_KEYS)
        );
        // owner-only.
        assert_eq!(
            field_authz("updateAutoBackupSettings"),
            FieldAuthz::OwnerOnly
        );
        assert_eq!(field_authz("backup"), FieldAuthz::OwnerOnly);
        assert_eq!(field_authz("restore"), FieldAuthz::OwnerOnly);
        assert_eq!(
            field_authz("completeFinancialSetupOnboarding"),
            FieldAuthz::OwnerOnly
        );
        // Go WithSystemBypass reads / self-service.
        assert_eq!(field_authz("me"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("myBalance"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("redeemCreditCode"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("myPrimaryProject"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("myProjectBalance"), FieldAuthz::Authenticated);
        assert_eq!(
            field_authz("myProjectWalletComparison"),
            FieldAuthz::Authenticated
        );
        assert_eq!(field_authz("mySubscriptions"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("myModelCatalog"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("systemStatus"), FieldAuthz::Authenticated);
        assert_eq!(
            field_authz("systemGeneralSettings"),
            FieldAuthz::Authenticated
        );
        assert_eq!(
            field_authz("updateSystemGeneralSettings"),
            FieldAuthz::Scope(slug::WRITE_SETTINGS)
        );
        assert_eq!(
            field_authz("productExperienceSettings"),
            FieldAuthz::Authenticated
        );
        assert_eq!(
            field_authz("updateProductExperienceSettings"),
            FieldAuthz::OwnerOnly
        );
        // Commercial resources have independent system scopes so operators do
        // not inherit unrelated user/settings/channel administration rights.
        assert_eq!(
            field_authz("subscriptionProjects"),
            FieldAuthz::Scope(slug::READ_SUBSCRIPTIONS)
        );
        assert_eq!(
            field_authz("projectBalance"),
            FieldAuthz::Scope(slug::READ_BILLING)
        );
        assert_eq!(
            field_authz("projectWalletComparison"),
            FieldAuthz::Scope(slug::READ_BILLING)
        );
        assert_eq!(
            field_authz("creditRedemptionCodes"),
            FieldAuthz::Scope(slug::READ_BILLING)
        );
        assert_eq!(
            field_authz("grantProjectCredit"),
            FieldAuthz::Scope(slug::GRANT_CREDIT)
        );
        assert_eq!(
            field_authz("createCreditRedemptionCodes"),
            FieldAuthz::Scope(slug::GRANT_CREDIT)
        );
        assert_eq!(
            field_authz("revokeCreditRedemptionCode"),
            FieldAuthz::Scope(slug::GRANT_CREDIT)
        );
        assert_eq!(
            field_authz("updateSubscriptionPlan"),
            FieldAuthz::Scope(slug::WRITE_SUBSCRIPTIONS)
        );
        assert_eq!(
            field_authz("simpleGroups"),
            FieldAuthz::Scope(slug::READ_GROUPS)
        );
        assert_eq!(
            field_authz("createSimpleGroup"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("updateSimpleGroup"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("assignSimpleGroupUsers"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("updateSimpleGroupModels"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("updateSimpleGroupPrice"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("deleteSimpleGroup"),
            FieldAuthz::Scope(slug::WRITE_GROUPS)
        );
        assert_eq!(
            field_authz("previewPromptProtectionRule"),
            FieldAuthz::Authenticated
        );
        assert_eq!(field_authz("health"), FieldAuthz::Public);
        // An unmapped field fails closed.
        assert_eq!(field_authz("somethingNew"), FieldAuthz::Deny);
    }

    #[test]
    fn internal_admin_documented_operation_scope_contract_is_stable() {
        let documented = [
            ("users", FieldAuthz::Scope(slug::READ_USERS)),
            ("createUser", FieldAuthz::Scope(slug::WRITE_USERS)),
            ("updateUser", FieldAuthz::Scope(slug::WRITE_USERS)),
            ("simpleGroups", FieldAuthz::Scope(slug::READ_GROUPS)),
            ("createSimpleGroup", FieldAuthz::Scope(slug::WRITE_GROUPS)),
            (
                "subscriptionPlans",
                FieldAuthz::Scope(slug::READ_SUBSCRIPTIONS),
            ),
            (
                "assignUserSubscription",
                FieldAuthz::Scope(slug::WRITE_SUBSCRIPTIONS),
            ),
            (
                "cancelUserSubscription",
                FieldAuthz::Scope(slug::WRITE_SUBSCRIPTIONS),
            ),
            ("projectBalance", FieldAuthz::Scope(slug::READ_BILLING)),
            ("grantProjectCredit", FieldAuthz::Scope(slug::GRANT_CREDIT)),
            (
                "creditRedemptionCodes",
                FieldAuthz::Scope(slug::READ_BILLING),
            ),
            (
                "createCreditRedemptionCodes",
                FieldAuthz::Scope(slug::GRANT_CREDIT),
            ),
            (
                "revokeCreditRedemptionCode",
                FieldAuthz::Scope(slug::GRANT_CREDIT),
            ),
            ("apiKeys", FieldAuthz::Scope(slug::READ_API_KEYS)),
            (
                "updateAPIKeyProfiles",
                FieldAuthz::Scope(slug::WRITE_API_KEYS),
            ),
            (
                "channelDiscoveryOperation",
                FieldAuthz::Scope(slug::READ_CHANNELS),
            ),
            ("channels", FieldAuthz::Scope(slug::READ_CHANNELS)),
            ("models", FieldAuthz::Scope(slug::READ_CHANNELS)),
        ];

        for (field, expected) in documented {
            assert_eq!(field_authz(field), expected, "scope drift for {field}");
        }
    }

    // ---- end-to-end through the extension ---------------------------------

    // A stand-in root type NAMED `QueryRoot` (via `#[Object(name = ...)]`) so the
    // extension's `parent_type` gate fires exactly as on the real schema.
    struct AuthzTestQuery;

    #[Object(name = "QueryRoot")]
    impl AuthzTestQuery {
        /// Maps to `Scope(WRITE_CHANNELS)` in the table.
        async fn create_channel(&self) -> i32 {
            1
        }
        /// `Scope(READ_API_KEYS)`.
        async fn api_keys(&self) -> i32 {
            2
        }
        /// `Scope(READ_REQUESTS)` and project-aware when the request context
        /// carries a selected project.
        async fn requests(&self) -> i32 {
            5
        }
        /// `Public`.
        async fn health(&self) -> &'static str {
            "ok"
        }
        /// `Authenticated`.
        async fn me(&self) -> i32 {
            3
        }
        /// Accounting display settings are available to every signed-in user.
        async fn system_general_settings(&self) -> i32 {
            8
        }
        /// The corresponding write remains protected by `write_settings`.
        async fn update_system_general_settings(&self) -> i32 {
            9
        }
        /// `OwnerOnly` (renamed to match the table key).
        #[graphql(name = "updateAutoBackupSettings")]
        async fn update_auto_backup_settings(&self) -> i32 {
            4
        }
        #[graphql(name = "grantProjectCredit")]
        async fn grant_project_credit(&self) -> i32 {
            6
        }
        #[graphql(name = "updateSubscriptionPlan")]
        async fn update_subscription_plan(&self) -> i32 {
            7
        }
        #[graphql(name = "simpleGroups")]
        async fn simple_groups(&self) -> i32 {
            10
        }
    }

    struct BrokenMode;
    struct SimpleMode;

    #[async_trait::async_trait]
    impl ProductExperienceServices for SimpleMode {
        async fn settings(
            &self,
        ) -> Result<
            crate::product_experience::ProductExperienceSettings,
            crate::product_experience::ProductExperienceError,
        > {
            Ok(crate::product_experience::ProductExperienceSettings {
                mode: ProductMode::Simple,
                ..Default::default()
            })
        }
        async fn update_settings(
            &self,
            _: crate::product_experience::UpdateProductExperienceSettingsInput,
        ) -> Result<
            crate::product_experience::ProductExperienceSettings,
            crate::product_experience::ProductExperienceError,
        > {
            self.settings().await
        }
    }

    #[async_trait::async_trait]
    impl ProductExperienceServices for BrokenMode {
        async fn settings(
            &self,
        ) -> Result<
            crate::product_experience::ProductExperienceSettings,
            crate::product_experience::ProductExperienceError,
        > {
            Err(crate::product_experience::ProductExperienceError::Read(
                "mode-store-unavailable".into(),
            ))
        }
        async fn update_settings(
            &self,
            _: crate::product_experience::UpdateProductExperienceSettingsInput,
        ) -> Result<
            crate::product_experience::ProductExperienceSettings,
            crate::product_experience::ProductExperienceError,
        > {
            self.settings().await
        }
    }

    #[tokio::test]
    async fn enterprise_guard_preserves_settings_read_error() {
        let service: Arc<dyn ProductExperienceServices> = Arc::new(BrokenMode);
        let schema = crate::admin_schema_builder()
            .extension(ScopeAuthExtensionFactory)
            .data(service)
            .finish();
        let response = schema
            .execute(Request::new("{ roles { totalCount } }").data(ctx(Some(
                Principal::user("reader").with_scope(slug::READ_ROLES),
            ))))
            .await;
        assert_eq!(response.errors.len(), 1);
        assert!(
            response.errors[0]
                .message
                .contains("mode-store-unavailable"),
            "{:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn simple_mode_blocks_scoped_reader_after_scope_check() {
        let service: Arc<dyn ProductExperienceServices> = Arc::new(SimpleMode);
        let schema = crate::admin_schema_builder()
            .extension(ScopeAuthExtensionFactory)
            .data(service)
            .finish();
        let allowed_scope = schema
            .execute(Request::new("{ roles { totalCount } }").data(ctx(Some(
                Principal::user("reader").with_scope(slug::READ_ROLES),
            ))))
            .await;
        assert!(
            allowed_scope.errors[0]
                .message
                .contains("only available in enterprise mode")
        );
        let no_scope = schema
            .execute(
                Request::new("{ roles { totalCount } }").data(ctx(Some(Principal::user("reader")))),
            )
            .await;
        assert!(no_scope.errors[0].message.contains("read_roles"));
        assert!(!no_scope.errors[0].message.contains("enterprise mode"));
    }

    fn guarded_schema() -> Schema<AuthzTestQuery, EmptyMutation, EmptySubscription> {
        Schema::build(AuthzTestQuery, EmptyMutation, EmptySubscription)
            .extension(ScopeAuthExtensionFactory)
            .finish()
    }

    fn ctx(principal: Option<Principal>) -> RequestContext {
        let mut rc = RequestContext::new();
        if let Some(p) = principal {
            let _ = rc.set_principal(p);
        }
        rc
    }

    async fn run(query: &str, principal: Option<Principal>) -> async_graphql::Response {
        guarded_schema()
            .execute(Request::new(query).data(ctx(principal)))
            .await
    }

    async fn run_for_project(
        query: &str,
        principal: Principal,
        project_id: &str,
    ) -> async_graphql::Response {
        let mut request_context = ctx(Some(principal));
        let _ = request_context.set_project_id(project_id);
        guarded_schema()
            .execute(Request::new(query).data(request_context))
            .await
    }

    #[tokio::test]
    async fn no_principal_is_denied() {
        let resp = run("{ createChannel }", None).await;
        assert!(!resp.errors.is_empty(), "no principal must be denied");
    }

    #[tokio::test]
    async fn owner_bypasses_scope() {
        let resp = run(
            "{ createChannel }",
            Some(Principal::user("1").with_owner(true)),
        )
        .await;
        assert!(
            resp.errors.is_empty(),
            "owner must bypass: {:?}",
            resp.errors
        );
    }

    #[tokio::test]
    async fn matching_scope_is_allowed() {
        let resp = run(
            "{ createChannel }",
            Some(Principal::user("2").with_scope(slug::WRITE_CHANNELS)),
        )
        .await;
        assert!(
            resp.errors.is_empty(),
            "write_channels holder allowed: {:?}",
            resp.errors
        );
    }

    #[tokio::test]
    async fn matching_project_role_scope_is_allowed_only_for_selected_project() {
        let principal = Principal::user("2").with_scope(conduit_auth::Scope::project_role(
            "project-1",
            slug::READ_REQUESTS,
        ));
        let allowed = run_for_project("{ requests }", principal.clone(), "project-1").await;
        assert!(
            allowed.errors.is_empty(),
            "matching project role scope must be allowed: {:?}",
            allowed.errors
        );

        let denied = run_for_project("{ requests }", principal, "project-2").await;
        assert!(
            !denied.errors.is_empty(),
            "a project role scope must not cross project boundaries"
        );
    }

    #[tokio::test]
    async fn project_owner_wildcard_cannot_authorize_global_mutations() {
        let principal = Principal::user("project-owner").with_scope(
            conduit_auth::Scope::project_membership("project-1", slug::WILDCARD),
        );

        for query in [
            "{ grantProjectCredit }",
            "{ updateSubscriptionPlan }",
            "{ updateSystemGeneralSettings }",
        ] {
            let response = run_for_project(query, principal.clone(), "project-1").await;
            assert!(
                !response.errors.is_empty(),
                "project ownership must not authorize global operation {query}"
            );
        }

        let project_response = run_for_project("{ requests }", principal, "project-1").await;
        assert!(
            project_response.errors.is_empty(),
            "project ownership must retain project-scoped access: {:?}",
            project_response.errors
        );
    }

    #[tokio::test]
    async fn system_role_scope_remains_valid_when_project_header_is_present() {
        let principal = Principal::user("credit-operator")
            .with_scope(conduit_auth::Scope::system_role(slug::GRANT_CREDIT));

        let response = run_for_project("{ grantProjectCredit }", principal, "project-1").await;
        assert!(
            response.errors.is_empty(),
            "system role must authorize global operation despite project header: {:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn wrong_scope_is_denied() {
        // Holds read_api_keys but asks for a write_channels field.
        let resp = run(
            "{ createChannel }",
            Some(Principal::user("3").with_scope(slug::READ_API_KEYS)),
        )
        .await;
        assert!(!resp.errors.is_empty(), "wrong scope must be denied");
    }

    #[tokio::test]
    async fn commercial_scopes_do_not_expand_into_unrelated_resources() {
        let credit_operator = Principal::user("credit").with_scope(slug::GRANT_CREDIT);
        let grant = run("{ grantProjectCredit }", Some(credit_operator.clone())).await;
        assert!(
            grant.errors.is_empty(),
            "grant scope must allow credit grant"
        );
        let subscription = run("{ updateSubscriptionPlan }", Some(credit_operator)).await;
        assert!(
            !subscription.errors.is_empty(),
            "grant scope must not allow subscription management"
        );

        let subscription_operator =
            Principal::user("subscriptions").with_scope(slug::WRITE_SUBSCRIPTIONS);
        let update = run(
            "{ updateSubscriptionPlan }",
            Some(subscription_operator.clone()),
        )
        .await;
        assert!(
            update.errors.is_empty(),
            "write_subscriptions must allow plan updates"
        );
        let grant = run("{ grantProjectCredit }", Some(subscription_operator)).await;
        assert!(
            !grant.errors.is_empty(),
            "subscription scope must not allow credit grants"
        );

        let group_reader = Principal::user("groups").with_scope(slug::READ_GROUPS);
        let groups = run("{ simpleGroups }", Some(group_reader)).await;
        assert!(
            groups.errors.is_empty(),
            "read_groups must independently allow group reads"
        );
    }

    #[tokio::test]
    async fn public_field_needs_no_auth() {
        let resp = run("{ health }", None).await;
        assert!(
            resp.errors.is_empty(),
            "public field allowed: {:?}",
            resp.errors
        );
    }

    #[tokio::test]
    async fn authenticated_field_needs_a_principal() {
        let denied = run("{ me }", None).await;
        assert!(!denied.errors.is_empty(), "me without principal denied");
        let ok = run("{ me }", Some(Principal::user("4"))).await;
        assert!(
            ok.errors.is_empty(),
            "me with principal allowed: {:?}",
            ok.errors
        );
    }

    #[tokio::test]
    async fn accounting_display_settings_are_readable_by_any_authenticated_user() {
        let denied = run("{ systemGeneralSettings }", None).await;
        assert!(
            !denied.errors.is_empty(),
            "anonymous accounting-settings read must be denied"
        );

        let allowed = run(
            "{ systemGeneralSettings }",
            Some(Principal::user("accounting-reader")),
        )
        .await;
        assert!(
            allowed.errors.is_empty(),
            "authenticated accounting-settings read must be allowed: {:?}",
            allowed.errors
        );
    }

    #[tokio::test]
    async fn accounting_settings_write_still_requires_write_settings() {
        let denied = run(
            "{ updateSystemGeneralSettings }",
            Some(Principal::user("accounting-reader")),
        )
        .await;
        assert!(
            !denied.errors.is_empty(),
            "an authenticated reader must not update accounting settings"
        );

        let allowed = run(
            "{ updateSystemGeneralSettings }",
            Some(Principal::user("accounting-writer").with_scope(slug::WRITE_SETTINGS)),
        )
        .await;
        assert!(
            allowed.errors.is_empty(),
            "write_settings holder must be allowed: {:?}",
            allowed.errors
        );
    }

    #[tokio::test]
    async fn owner_only_field_requires_owner() {
        // A scoped non-owner is denied; an owner is allowed.
        let denied = run(
            "{ updateAutoBackupSettings }",
            Some(Principal::user("5").with_scope(slug::WRITE_SETTINGS)),
        )
        .await;
        assert!(
            !denied.errors.is_empty(),
            "non-owner must be denied for owner-only field"
        );
        let ok = run(
            "{ updateAutoBackupSettings }",
            Some(Principal::user("6").with_owner(true)),
        )
        .await;
        assert!(ok.errors.is_empty(), "owner allowed: {:?}", ok.errors);
    }

    #[tokio::test]
    async fn introspection_is_exempt() {
        // `__typename` is introspection → never gated, even with no principal.
        let resp = run("{ __typename }", None).await;
        assert!(
            resp.errors.is_empty(),
            "introspection exempt: {:?}",
            resp.errors
        );
    }
}

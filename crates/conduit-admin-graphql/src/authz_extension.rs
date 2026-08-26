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
use conduit_auth::scopes::slug;
use std::sync::Arc;

use crate::policy::{authorize_project_resolver, authorize_resolver};

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
        "createChannel" | "updateChannel" | "deleteChannel" | "updateChannelStatus"
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
        "testChannel" | "testChannelAPIKey" | "testChannelAPIKeys" | "channels" | "models"
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
        "createSimpleGroup" | "updateSimpleGroup" | "assignSimpleGroupUsers"
        | "updateSimpleGroupModels" | "updateSimpleGroupPrice" | "deleteSimpleGroup" => {
            Scope(slug::WRITE_GROUPS)
        }
        "simpleGroups" => Scope(slug::READ_GROUPS),

        "publicChannelHealth" | "myModelCatalog" => Authenticated,
        "publicChannelHealthSettings" => Scope(slug::READ_SETTINGS),
        "updatePublicChannelHealthSettings" => Scope(slug::WRITE_SETTINGS),

        // ── billing + subscriptions ────────────────────────────────────────
        "userBalance" | "projectBalance" | "projectWalletComparison" => {
            Scope(slug::READ_BILLING)
        }
        "userSubscriptions" | "subscriptionProjects" | "subscriptionPlans" => {
            Scope(slug::READ_SUBSCRIPTIONS)
        }
        "myBalance" | "mySubscriptions" | "myProjectBalance"
        | "myProjectWalletComparison" | "myPrimaryProject" => Authenticated,
        "grantUserCredit" | "grantProjectCredit" => Scope(slug::GRANT_CREDIT),
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
        "createPriceBook" | "createProviderPriceChangeSet" | "createRetailPriceChangeSet"
        | "saveChannelModelPrices" | "saveRetailPriceChangeSetItem"
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
        "priceBooks" | "previewCustomerCharge" | "changeSets" => {
            Scope(slug::READ_COMMERCIALIZATION)
        }

        "clearCache" | "getCacheDiagnostics" | "updateProductExperienceSettings" => OwnerOnly,

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
            if principal.is_some() {
                Ok(())
            } else {
                Err(NO_PRINCIPAL.to_string())
            }
        }
        FieldAuthz::Scope(scope) => {
            let Some(principal) = principal else {
                return Err(NO_PRINCIPAL.to_string());
            };
            if let Some(project_id) = request_context.and_then(|rc| rc.project_id.as_deref()) {
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

    #[test]
    fn table_maps_entities_to_the_right_scopes() {
        assert_eq!(
            field_authz("createChannel"),
            FieldAuthz::Scope(slug::WRITE_CHANNELS)
        );
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
        // Go WithSystemBypass reads / self-service.
        assert_eq!(field_authz("me"), FieldAuthz::Authenticated);
        assert_eq!(field_authz("myBalance"), FieldAuthz::Authenticated);
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
            field_authz("grantProjectCredit"),
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
            ("apiKeys", FieldAuthz::Scope(slug::READ_API_KEYS)),
            (
                "updateAPIKeyProfiles",
                FieldAuthz::Scope(slug::WRITE_API_KEYS),
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

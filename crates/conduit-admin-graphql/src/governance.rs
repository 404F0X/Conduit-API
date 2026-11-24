//! Governance transports typed commands; policy, permissions and transaction
//! application are checked by the production adapter independently of GraphQL.
use crate::{change_package::ChangeSetDetail, change_set::ChangeSetKind};
use async_graphql::{Context, ID, InputObject, Json, SimpleObject};
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, Clone, SimpleObject)]
pub struct GovernanceSettings {
    pub revision: String,
    pub enabled: bool,
    pub review_kinds: Vec<ChangeSetKind>,
    pub allow_self_review: bool,
    pub legacy_review_scopes: bool,
}
#[derive(Debug, Clone, InputObject)]
pub struct UpdateGovernanceSettingsInput {
    pub enabled: bool,
    pub review_kinds: Vec<ChangeSetKind>,
    pub allow_self_review: Option<bool>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct GovernedActionCapability {
    pub action: String,
    pub kind: ChangeSetKind,
    pub domain_scope: String,
    pub supported: bool,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct GovernanceCapabilities {
    pub actions: Vec<GovernedActionCapability>,
    pub review_scope: String,
    pub publish_scope: String,
    pub policy_scope: String,
    pub eligible_reviewer_count: i32,
}
#[derive(Clone, InputObject)]
pub struct ProposeGovernedChangeInput {
    pub action: String,
    #[graphql(name = "targetIDs")]
    pub target_ids: Vec<ID>,
    pub payload: Json<serde_json::Value>,
    pub expected_target_revision: String,
    #[graphql(name = "operationID")]
    pub operation_id: ID,
    pub reason: String,
}
#[async_trait]
pub trait GovernanceServices: Send + Sync {
    async fn check_direct(&self, actor: i64, field: &str) -> Result<(), String>;
    async fn settings(&self) -> Result<GovernanceSettings, String>;
    async fn capabilities(&self, actor: i64) -> Result<GovernanceCapabilities, String>;
    async fn target_revision(
        &self,
        actor: i64,
        action: String,
        target_ids: Vec<ID>,
        payload: Json<serde_json::Value>,
    ) -> Result<String, String>;
    async fn readable_kinds(&self, actor: i64) -> Result<Vec<ChangeSetKind>, String>;
    async fn authorize_read(&self, actor: i64, id: ID) -> Result<(), String>;
    async fn update_settings(
        &self,
        actor: i64,
        input: UpdateGovernanceSettingsInput,
        expected: String,
    ) -> Result<GovernanceSettings, String>;
    async fn propose(
        &self,
        actor: i64,
        input: ProposeGovernedChangeInput,
    ) -> Result<ChangeSetDetail, String>;
    async fn validate(
        &self,
        actor: i64,
        id: ID,
        expected: String,
    ) -> Result<ChangeSetDetail, String>;
    async fn recover(
        &self,
        actor: i64,
        reason: String,
        expected: String,
    ) -> Result<GovernanceSettings, String>;
    async fn claim_secrets(&self, actor: i64, id: ID) -> Result<Json<serde_json::Value>, String>;
    async fn emergency(
        &self,
        actor: i64,
        target_type: String,
        id: ID,
        reason: String,
    ) -> Result<bool, String>;
}
pub(crate) fn services(ctx: &Context<'_>) -> Result<Arc<dyn GovernanceServices>, String> {
    ctx.data::<Arc<dyn GovernanceServices>>()
        .cloned()
        .map_err(|_| "governance services unavailable".into())
}
pub(crate) fn actor(ctx: &Context<'_>) -> Result<i64, String> {
    ctx.data::<crate::me::CurrentUser>()
        .map(|u| u.user_id)
        .map_err(|_| "authentication required".into())
}

/// Every live mutation has one classification. Unknown writes fail closed even
/// when approval is disabled. Domain policy is rechecked by transaction gates.
pub fn mutation_kind(field: &str) -> Result<Option<ChangeSetKind>, String> {
    use ChangeSetKind::*;
    let exception = matches!(
        field,
        "claimGovernedChangeSecrets"
            | "proposeGovernedChange"
            | "validateChangeSet"
            | "reviewChangeSet"
            | "publishChangeSet"
            | "updateGovernanceSettings"
            | "recoverGovernancePolicy"
            | "emergencyRestrictAccess"
            | "createProviderPriceChangeSet"
            | "saveProviderPriceChangeSet"
            | "createRetailPriceChangeSet"
            | "saveRetailPriceChangeSetItem"
            | "saveRetailPriceFormula"
            | "submitChangeSet"
            | "approveChangeSet"
            | "rejectChangeSet"
            | "initializeNewApiChannel"
            | "retryNewApiChannelDiscovery"
            | "syncChannelModels"
            | "resetChannelQuotaNow"
            | "confirmChannelQuotaProbe"
            | "redeemCreditCode"
            // Customer-funded receipts and verified gateway reconciliation do
            // not grant credit or alter the frozen target. Keep the same
            // operational boundary as redemption; authorization still applies.
            | "createPaymentOrder"
            | "recheckPaymentOrder"
            | "operatorRecheckPaymentOrder"
            | "grantUserCredit"
            | "checkProviderQuotas"
            | "probeChannelQuota"
            | "probeNewApiPricing"
            | "testChannel"
            | "testChannelAPIKey"
            | "testChannelAPIKeys"
            | "previewPromptProtectionRule"
            | "updateMe"
            | "updateMyPassword"
            | "unlinkOIDCIdentity"
            | "createPrompt"
            | "updatePrompt"
            | "deletePrompt"
            | "updatePromptStatus"
            | "bulkDeletePrompts"
            | "bulkEnablePrompts"
            | "bulkDisablePrompts"
            | "backup"
            | "restore"
            | "triggerAutoBackup"
            | "triggerGcCleanup"
            | "clearCache"
            | "completeOnboarding"
            | "completeFinancialSetupOnboarding"
            | "completeSystemModelSettingOnboarding"
            | "completeAutoDisableChannelOnboarding"
            | "updateProductExperienceSettings"
            | "createAPIKey"
            | "updateAPIKey"
            | "updateAPIKeyStatus"
            | "rotateAPIKey"
            | "updateAPIKeyProfiles"
            // Select an already-authorized group for one key, like updating
            // its profile; this does not change global group configuration.
            | "assignApiKeyModelGroup"
            | "bulkDisableAPIKeys"
            | "bulkEnableAPIKeys"
            | "bulkArchiveAPIKeys"
    );
    if exception {
        return Ok(None);
    }
    if field == "grantProjectCredit" {
        return Ok(Some(ProjectCredit));
    }
    if field == "saveChannelModelPrices" {
        return Ok(Some(ProviderPrice));
    }
    if field == "createPriceBook" {
        return Ok(Some(RetailPrice));
    }
    if matches!(
        field,
        "applyChannelModelMappings" | "setChannelModelMappingAutomation"
    ) {
        return Ok(Some(ModelMapping));
    }
    if matches!(
        field,
        "upsertModelRoute"
            | "createPublicModelWithRoutes"
            | "createModel"
            | "updateModel"
            | "deleteModel"
            | "updateModelStatus"
            | "bulkCreateModels"
            | "bulkArchiveModels"
            | "bulkDisableModels"
            | "bulkEnableModels"
            | "bulkDeleteModels"
    ) {
        return Ok(Some(ModelSupply));
    }
    if matches!(
        field,
        "createSimpleGroup"
            | "saveModelGroup"
            | "saveUserGroup"
            | "moveUserToGroup"
            | "updateSimpleGroup"
            | "assignSimpleGroupUsers"
            | "updateSimpleGroupModels"
            | "updateSimpleGroupPrice"
            | "deleteSimpleGroup"
    ) {
        return Ok(Some(GroupAccess));
    }
    if matches!(
        field,
        "createProject"
            | "updateProject"
            | "updateProjectStatus"
            | "updateProjectProfiles"
            | "deleteProject"
            | "setProjectMemberSpendLimit"
    ) {
        return Ok(Some(ProjectConfiguration));
    }
    if matches!(
        field,
        "createUser"
            | "updateUser"
            | "updateUserStatus"
            | "deleteUser"
            | "addUserToProject"
            | "removeUserFromProject"
            | "updateProjectUser"
            | "createRole"
            | "updateRole"
            | "deleteRole"
            | "bulkDeleteRoles"
            | "createApiKeyProfileTemplate"
            | "updateApiKeyProfileTemplate"
            | "deleteApiKeyProfileTemplate"
            | "loadApiKeyProfileTemplate"
    ) {
        return Ok(Some(IdentityAccess));
    }
    if matches!(
        field,
        "createChannel"
            | "updateChannel"
            | "deleteChannel"
            | "duplicateChannel"
            | "updateChannelStatus"
            | "saveChannelEndpoints"
            | "bulkArchiveChannels"
            | "bulkDisableChannels"
            | "bulkEnableChannels"
            | "bulkRecoverChannels"
            | "bulkDeleteChannels"
            | "bulkCreateChannels"
            | "bulkImportChannels"
            | "bulkUpdateChannelOrdering"
            | "disableChannelAPIKey"
            | "enableChannelAPIKey"
            | "enableAllChannelAPIKeys"
            | "enableSelectedChannelAPIKeys"
            | "deleteDisabledChannelAPIKeys"
            | "createChannelOverrideTemplate"
            | "updateChannelOverrideTemplate"
            | "deleteChannelOverrideTemplate"
            | "applyChannelOverrideTemplate"
            | "clearChannelOverrideTemplates"
    ) {
        return Ok(Some(ChannelConfiguration));
    }
    if matches!(
        field,
        "createSubscriptionPlan"
            | "updateSubscriptionPlan"
            | "assignUserSubscription"
            | "refreshSubscriptionAllowance"
            | "pauseUserSubscription"
            | "resumeUserSubscription"
            | "cancelUserSubscription"
            | "renewUserSubscription"
            | "setSubscriptionAutoRenew"
            | "createCreditRedemptionCodes"
            | "revokeCreditRedemptionCode"
            | "createPromptProtectionRule"
            | "updatePromptProtectionRule"
            | "deletePromptProtectionRule"
            | "updatePromptProtectionRuleStatus"
            | "bulkDeletePromptProtectionRules"
            | "bulkEnablePromptProtectionRules"
            | "bulkDisablePromptProtectionRules"
            | "createDataStorage"
            | "updateDataStorage"
            | "completeFinancialSetupOnboarding"
            | "completeSystemModelSettingOnboarding"
            | "completeAutoDisableChannelOnboarding"
            | "updateSecuritySettings"
            | "saveProxyPreset"
            | "deleteProxyPreset"
            | "updateBrandSettings"
            | "updateStoragePolicy"
            | "updateRetryPolicy"
            | "updateUserAgentPassThroughSettings"
            | "updateDefaultDataStorage"
            | "updateSystemGeneralSettings"
            | "updateSystemModelSettings"
            | "updateSystemChannelSettings"
            | "updatePublicChannelHealthSettings"
            | "updatePassThroughSettings"
            | "updateQuotaEnforcementSettings"
            | "updateVideoStorageSettings"
            | "updateWebhookNotifierConfig"
            | "updateAutoBackupSettings"
    ) {
        return Ok(Some(SystemConfiguration));
    }
    Err("GOVERNANCE_UNSUPPORTED: unclassified mutation".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn group_and_payment_mutations_preserve_existing_governance_domains() {
        use crate::change_set::ChangeSetKind;
        for field in ["saveModelGroup", "saveUserGroup", "moveUserToGroup"] {
            assert_eq!(
                super::mutation_kind(field).unwrap(),
                Some(ChangeSetKind::GroupAccess)
            );
        }
        for field in [
            "assignApiKeyModelGroup",
            "createPaymentOrder",
            "recheckPaymentOrder",
            "operatorRecheckPaymentOrder",
        ] {
            assert_eq!(super::mutation_kind(field).unwrap(), None);
        }
        assert!(super::mutation_kind("unexpectedNewWrite").is_err());
    }
    #[tokio::test]
    async fn every_real_mutation_is_classified_and_new_fields_fail_closed() {
        let response = crate::build_admin_schema()
            .execute("{ __type(name: \"MutationRoot\") { fields { name } } }")
            .await;
        assert!(response.errors.is_empty());
        let data = response.data.into_json().expect("introspection");
        let fields = data["__type"]["fields"]
            .as_array()
            .expect("mutation fields");
        assert!(fields.len() > 150);
        for field in fields {
            let name = field["name"].as_str().expect("field name");
            assert!(super::mutation_kind(name).is_ok(), "unclassified {name}");
        }
        assert!(super::mutation_kind("unexpectedNewWrite").is_err());
    }
}

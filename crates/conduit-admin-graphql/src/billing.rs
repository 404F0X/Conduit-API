//! User Credit and subscription allowance admin API (Rust extension).

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum BillingStatus {
    Enabled,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum SubscriptionIntervalUnit {
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RolloverMode {
    None,
    Capped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum QuotaClass {
    General,
    Dedicated,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct SubscriptionQuotaRule {
    pub id: ID,
    pub name: String,
    pub quota_class: QuotaClass,
    pub allowance: String,
    pub rollover_mode: RolloverMode,
    pub rollover_cap: Option<String>,
    pub carryover_days: Option<i32>,
    pub access_plans: Vec<SubscriptionAccessPlan>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct SubscriptionAllowanceBucket {
    pub id: ID,
    pub name: String,
    pub quota_class: QuotaClass,
    pub source_type: String,
    pub period_start: String,
    pub period_end: String,
    pub expires_at: String,
    pub granted_allowance: String,
    pub consumed_allowance: String,
    pub reserved_allowance: String,
    pub remaining_allowance: String,
    pub status: String,
    pub access_plans: Vec<SubscriptionAccessPlan>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<String>,
    #[graphql(name = "sourceBucketID")]
    pub source_bucket_id: Option<ID>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct CreditLedgerEntry {
    pub id: ID,
    pub amount: String,
    pub entry_type: String,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct UserBalance {
    #[graphql(name = "userID")]
    pub user_id: ID,
    pub currency: String,
    pub credit_balance: String,
    pub subscription_balance: String,
    pub general_subscription_balance: String,
    pub dedicated_subscription_balance: String,
    pub reserved_balance: String,
    pub available_balance: String,
    pub ledger_entries: Vec<CreditLedgerEntry>,
}

/// Project-owned balance calculated from the independent shadow ledger plus
/// subscriptions explicitly linked to this Project.
#[derive(Debug, Clone, SimpleObject)]
pub struct ProjectBalance {
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub currency: String,
    pub wallet_status: String,
    pub credit_balance: String,
    pub subscription_balance: String,
    pub general_subscription_balance: String,
    pub dedicated_subscription_balance: String,
    pub reserved_balance: String,
    pub available_balance: String,
    pub ledger_entries: Vec<CreditLedgerEntry>,
}

/// Read-only shadow comparison. It never copies or mutates legacy funds.
#[derive(Debug, Clone, SimpleObject)]
pub struct ProjectWalletComparison {
    #[graphql(name = "projectID")]
    pub project_id: ID,
    #[graphql(name = "ownerUserID")]
    pub owner_user_id: Option<ID>,
    pub status: String,
    pub legacy_credit_balance: String,
    pub project_credit_balance: String,
    pub legacy_subscription_balance: String,
    pub project_subscription_balance: String,
    pub legacy_available_balance: String,
    pub project_available_balance: String,
    pub available_delta: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct SubscriptionAccessPlan {
    pub id: ID,
    pub name: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct SubscriptionPlan {
    pub id: ID,
    pub name: String,
    pub currency: String,
    pub allowance: String,
    pub interval_unit: SubscriptionIntervalUnit,
    pub interval_count: i32,
    pub rollover_mode: RolloverMode,
    pub rollover_cap: Option<String>,
    /// Model groups are represented internally by their Access Plans. A
    /// subscription plan may grant any number of them.
    pub access_plans: Vec<SubscriptionAccessPlan>,
    pub quota_rules: Vec<SubscriptionQuotaRule>,
    pub status: BillingStatus,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct UserSubscription {
    pub id: ID,
    #[graphql(name = "userID")]
    pub user_id: ID,
    pub plan: SubscriptionPlan,
    pub status: String,
    pub current_period_start: String,
    pub current_period_end: String,
    pub auto_renew: bool,
    pub interval_unit: SubscriptionIntervalUnit,
    pub interval_count: i32,
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    /// Exact Access Plans whose published versions were snapshotted for the
    /// current subscription period.
    pub granted_access_plans: Vec<SubscriptionAccessPlan>,
    /// Simple-mode Group names backed by the granted Access Plan. Enterprise
    /// Access Plans that are not wrapped by a Group legitimately return none.
    pub granted_group_names: Vec<String>,
    /// Public model keys from the exact published Access Plan version granted
    /// to this subscription.
    #[graphql(name = "grantedModelIDs")]
    pub granted_model_ids: Vec<String>,
    pub granted_allowance: String,
    pub consumed_allowance: String,
    pub reserved_allowance: String,
    pub remaining_allowance: String,
    pub allowance_buckets: Vec<SubscriptionAllowanceBucket>,
    pub general_remaining_allowance: String,
    pub dedicated_remaining_allowance: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct SubscriptionProjectOption {
    pub id: ID,
    pub name: String,
    pub status: String,
    pub commercial_policy_active: bool,
}

#[derive(Debug, Clone, InputObject)]
pub struct GrantUserCreditInput {
    #[graphql(name = "userID")]
    pub user_id: ID,
    pub amount: String,
    pub currency: Option<String>,
    pub description: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, InputObject)]
pub struct GrantProjectCreditInput {
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub amount: String,
    pub description: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, InputObject)]
pub struct CreateSubscriptionPlanInput {
    pub name: String,
    pub interval_unit: SubscriptionIntervalUnit,
    pub interval_count: Option<i32>,
    #[graphql(name = "accessPlanIDs")]
    pub access_plan_ids: Vec<ID>,
    pub quota_rules: Vec<SubscriptionQuotaRuleInput>,
}

#[derive(Debug, Clone, InputObject)]
pub struct SubscriptionQuotaRuleInput {
    pub id: Option<ID>,
    pub name: String,
    pub quota_class: QuotaClass,
    pub allowance: String,
    pub rollover_mode: Option<RolloverMode>,
    pub rollover_cap: Option<String>,
    pub carryover_days: Option<i32>,
    #[graphql(name = "accessPlanIDs")]
    pub access_plan_ids: Vec<ID>,
}

#[derive(Debug, Clone, InputObject)]
pub struct UpdateSubscriptionPlanInput {
    pub id: ID,
    pub name: String,
    pub interval_unit: SubscriptionIntervalUnit,
    pub interval_count: i32,
    #[graphql(name = "accessPlanIDs")]
    pub access_plan_ids: Vec<ID>,
    pub quota_rules: Vec<SubscriptionQuotaRuleInput>,
    pub status: BillingStatus,
}

#[derive(Debug, Clone, InputObject)]
pub struct AssignUserSubscriptionInput {
    #[graphql(name = "userID")]
    pub user_id: ID,
    #[graphql(name = "planID")]
    pub plan_id: ID,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub idempotency_key: String,
    pub auto_renew: Option<bool>,
    pub interval_unit: Option<SubscriptionIntervalUnit>,
    pub interval_count: Option<i32>,
}

#[derive(Debug, Clone, InputObject)]
pub struct SetSubscriptionAutoRenewInput {
    #[graphql(name = "subscriptionID")]
    pub subscription_id: ID,
    pub auto_renew: bool,
}

/// Append-only security and business audit record for high-risk billing
/// mutations. IDs are kept in their original GraphQL wire form so a rejected
/// request with an invalid ID can still be recorded faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommercialOperationAudit {
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub operation: String,
    pub target_project_id: Option<String>,
    pub target_user_id: Option<String>,
    pub amount: Option<String>,
    pub currency: Option<String>,
    pub plan_id: Option<String>,
    pub plan_name: Option<String>,
    pub subscription_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub result: String,
    pub error_message: Option<String>,
}

impl CommercialOperationAudit {
    pub(crate) fn for_request(ctx: &Context<'_>, operation: impl Into<String>) -> Self {
        let principal =
            crate::policy::request_context(ctx).and_then(|request| request.principal.as_ref());
        Self {
            actor_type: principal
                .map(|principal| principal.kind.to_string())
                .unwrap_or_else(|| "unknown".into()),
            actor_id: principal.and_then(|principal| principal.id.clone()),
            operation: operation.into(),
            target_project_id: None,
            target_user_id: None,
            amount: None,
            currency: None,
            plan_id: None,
            plan_name: None,
            subscription_id: None,
            idempotency_key: None,
            result: "pending".into(),
            error_message: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BillingError {
    #[error("billing service is unavailable")]
    Unavailable,
    #[error("billing object not found: {0}")]
    NotFound(String),
    #[error("invalid billing input: {0}")]
    Invalid(String),
    #[error("billing operation failed: {0}")]
    Storage(String),
}

#[async_trait::async_trait]
pub trait BillingServices: Send + Sync {
    async fn user_balance(&self, user_id: &str) -> Result<UserBalance, BillingError>;
    async fn project_balance(&self, project_id: &str) -> Result<ProjectBalance, BillingError>;
    async fn project_wallet_comparison(
        &self,
        project_id: &str,
    ) -> Result<ProjectWalletComparison, BillingError>;
    /// Self-service Project wallet lookup. Implementations must verify that
    /// `user_id` is an active member of `project_id` before returning data.
    async fn user_project_balance(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<ProjectBalance, BillingError>;
    /// Self-service legacy/shadow comparison with the same membership guard as
    /// [`BillingServices::user_project_balance`].
    async fn user_project_wallet_comparison(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<ProjectWalletComparison, BillingError>;
    async fn subscription_plans(&self) -> Result<Vec<SubscriptionPlan>, BillingError>;
    async fn user_subscriptions(
        &self,
        user_id: &str,
    ) -> Result<Vec<UserSubscription>, BillingError>;
    /// Self-service subscription lookup scoped to one explicitly selected
    /// Project. Implementations must verify active membership before reading.
    async fn user_project_subscriptions(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<UserSubscription>, BillingError>;
    async fn subscription_projects(
        &self,
        user_id: &str,
    ) -> Result<Vec<SubscriptionProjectOption>, BillingError>;
    async fn grant_user_credit(
        &self,
        input: GrantUserCreditInput,
    ) -> Result<UserBalance, BillingError>;
    async fn grant_project_credit(
        &self,
        input: GrantProjectCreditInput,
    ) -> Result<ProjectBalance, BillingError>;
    async fn create_subscription_plan(
        &self,
        input: CreateSubscriptionPlanInput,
    ) -> Result<SubscriptionPlan, BillingError>;
    async fn update_subscription_plan(
        &self,
        input: UpdateSubscriptionPlanInput,
    ) -> Result<SubscriptionPlan, BillingError>;
    async fn assign_user_subscription(
        &self,
        input: AssignUserSubscriptionInput,
    ) -> Result<UserSubscription, BillingError>;
    async fn refresh_subscription_allowance(
        &self,
        subscription_id: &str,
    ) -> Result<UserSubscription, BillingError>;
    async fn pause_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<UserSubscription, BillingError>;
    async fn resume_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<UserSubscription, BillingError>;
    async fn cancel_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<UserSubscription, BillingError>;
    async fn renew_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<UserSubscription, BillingError>;
    async fn set_subscription_auto_renew(
        &self,
        input: SetSubscriptionAutoRenewInput,
    ) -> Result<UserSubscription, BillingError>;

    /// Persist an append-only audit row after a billing mutation completes.
    async fn record_commercial_operation_audit(
        &self,
        audit: CommercialOperationAudit,
    ) -> Result<(), BillingError>;
}

pub(crate) fn billing_services(ctx: &Context<'_>) -> Result<Arc<dyn BillingServices>, String> {
    ctx.data::<Arc<dyn BillingServices>>()
        .cloned()
        .map_err(|_| BillingError::Unavailable.to_string())
}

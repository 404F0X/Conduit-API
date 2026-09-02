//! User Credit and subscription allowance admin API (Rust extension).

use std::{str::FromStr, sync::Arc};

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

pub const DEFAULT_CREDIT_REDEMPTION_PAGE_LIMIT: i32 = 50;
pub const MAX_CREDIT_REDEMPTION_PAGE_LIMIT: i32 = 200;
pub const MAX_CREDIT_REDEMPTION_QUANTITY: i32 = 1_000;
pub const DEFAULT_CREDIT_REDEMPTIONS_PER_CODE: i32 = 1;
pub const MAX_CREDIT_REDEMPTIONS_PER_CODE: i32 = 100_000;
pub const MAX_CREDIT_REDEMPTION_DESCRIPTION_CHARS: usize = 500;
pub const CREDIT_REDEMPTION_CODE_MIN_CHARS: usize = 8;
pub const CREDIT_REDEMPTION_CODE_MAX_CHARS: usize = 128;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum CreditRedemptionCodeStatus {
    Active,
    Redeemed,
    Revoked,
    Expired,
}

/// Administrative view of a redemption code. The plaintext code is
/// intentionally absent; it is returned only once in
/// [`GeneratedCreditRedemptionCode`].
#[derive(Debug, Clone, SimpleObject)]
pub struct CreditRedemptionCode {
    pub id: ID,
    #[graphql(name = "batchID")]
    pub batch_id: ID,
    pub code_hint: String,
    pub amount: String,
    pub currency: String,
    pub description: Option<String>,
    pub max_redemptions: i32,
    pub redemption_count: i32,
    pub remaining_redemptions: i32,
    pub status: CreditRedemptionCodeStatus,
    pub expires_at: Option<String>,
    pub redeemed_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct CreditRedemptionCodePage {
    pub items: Vec<CreditRedemptionCode>,
    pub total: i32,
    pub limit: i32,
    pub offset: i32,
}

/// One-time secret returned by code creation. Deliberately does not derive
/// `Debug`, which keeps accidental structured logging from exposing `code`.
#[derive(Clone, SimpleObject)]
pub struct GeneratedCreditRedemptionCode {
    pub id: ID,
    pub code: String,
    pub code_hint: String,
}

#[derive(Clone, SimpleObject)]
pub struct CreateCreditRedemptionCodesPayload {
    #[graphql(name = "batchID")]
    pub batch_id: ID,
    pub amount: String,
    pub currency: String,
    pub quantity: i32,
    pub max_redemptions: i32,
    pub expires_at: Option<String>,
    pub codes: Vec<GeneratedCreditRedemptionCode>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct CreditRedemptionReceipt {
    pub id: ID,
    #[graphql(name = "codeID")]
    pub code_id: ID,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    #[graphql(name = "userID")]
    pub user_id: ID,
    pub amount: String,
    pub currency: String,
    pub redeemed_at: String,
}

#[derive(Debug, Clone, InputObject)]
pub struct CreateCreditRedemptionCodesInput {
    pub amount: String,
    pub quantity: i32,
    #[graphql(default = 1)]
    pub max_redemptions: i32,
    pub expires_at: Option<String>,
    pub description: Option<String>,
}

/// Authenticated actor metadata forwarded to the PostgreSQL adapter so the
/// state change and its audit row can commit in one transaction. It contains
/// no raw redemption code or other request secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditRedemptionActor {
    pub actor_type: String,
    pub actor_id: Option<String>,
}

impl CreditRedemptionActor {
    pub(crate) fn for_request(ctx: &Context<'_>) -> Result<Self, String> {
        let principal = crate::policy::request_context(ctx)
            .and_then(|request| request.principal.as_ref())
            .ok_or_else(|| "authentication required".to_string())?;
        Ok(Self {
            actor_type: principal.kind.to_string(),
            actor_id: principal.id.clone(),
        })
    }
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
    /// Public redemption failures intentionally collapse unknown, expired,
    /// redeemed, and revoked codes into one response to prevent enumeration.
    #[error("credit redemption code is invalid or unavailable")]
    RedemptionCodeUnavailable,
    #[error("billing operation failed: {0}")]
    Storage(String),
}

pub fn validate_credit_redemption_pagination(limit: i32, offset: i32) -> Result<(), BillingError> {
    if !(1..=MAX_CREDIT_REDEMPTION_PAGE_LIMIT).contains(&limit) {
        return Err(BillingError::Invalid(format!(
            "limit must be between 1 and {MAX_CREDIT_REDEMPTION_PAGE_LIMIT}"
        )));
    }
    if offset < 0 {
        return Err(BillingError::Invalid(
            "offset cannot be negative".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_create_credit_redemption_codes_input(
    input: &CreateCreditRedemptionCodesInput,
) -> Result<(), BillingError> {
    validate_credit_redemption_amount(&input.amount)?;
    if !(1..=MAX_CREDIT_REDEMPTION_QUANTITY).contains(&input.quantity) {
        return Err(BillingError::Invalid(format!(
            "quantity must be between 1 and {MAX_CREDIT_REDEMPTION_QUANTITY}"
        )));
    }
    let max_redemptions = input.max_redemptions;
    if !(1..=MAX_CREDIT_REDEMPTIONS_PER_CODE).contains(&max_redemptions) {
        return Err(BillingError::Invalid(format!(
            "maxRedemptions must be between 1 and {MAX_CREDIT_REDEMPTIONS_PER_CODE}"
        )));
    }
    if input
        .description
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_CREDIT_REDEMPTION_DESCRIPTION_CHARS)
    {
        return Err(BillingError::Invalid(format!(
            "description cannot exceed {MAX_CREDIT_REDEMPTION_DESCRIPTION_CHARS} characters"
        )));
    }
    if let Some(expires_at) = input.expires_at.as_deref() {
        let expires_at = DateTime::parse_from_rfc3339(expires_at.trim())
            .map_err(|_| BillingError::Invalid("expiresAt must be RFC 3339".to_string()))?
            .with_timezone(&Utc);
        if expires_at <= Utc::now() {
            return Err(BillingError::Invalid(
                "expiresAt must be in the future".to_string(),
            ));
        }
    }
    Ok(())
}

/// Accept a plain positive decimal without signs, exponent notation, or more
/// than six fractional digits. This exactly matches the micros ledger scale.
pub fn validate_credit_redemption_amount(value: &str) -> Result<(), BillingError> {
    let value = value.trim();
    let mut components = value.split('.');
    let integer = components.next().unwrap_or_default();
    let fraction = components.next();
    if value.is_empty()
        || integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|digits| {
            digits.is_empty()
                || digits.len() > 6
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
        })
        || components.next().is_some()
    {
        return Err(BillingError::Invalid(
            "amount must be a positive decimal with at most 6 fractional digits".to_string(),
        ));
    }
    let amount = Decimal::from_str(value).map_err(|_| {
        BillingError::Invalid(
            "amount must be a positive decimal with at most 6 fractional digits".to_string(),
        )
    })?;
    let largest = Decimal::from(i64::MAX) / Decimal::from(1_000_000_i64);
    if amount <= Decimal::ZERO || amount > largest {
        return Err(BillingError::Invalid(
            "amount must be positive and fit the credit ledger range".to_string(),
        ));
    }
    Ok(())
}

/// Normalize the human-entered code before hashing. All invalid shapes use the
/// same public error as unknown or unavailable codes.
pub fn normalize_credit_redemption_code(value: &str) -> Result<String, BillingError> {
    let code = value.trim().to_ascii_uppercase();
    if !(CREDIT_REDEMPTION_CODE_MIN_CHARS..=CREDIT_REDEMPTION_CODE_MAX_CHARS).contains(&code.len())
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(BillingError::RedemptionCodeUnavailable);
    }
    Ok(code)
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
    async fn credit_redemption_codes(
        &self,
        limit: i32,
        offset: i32,
    ) -> Result<CreditRedemptionCodePage, BillingError>;
    async fn create_credit_redemption_codes(
        &self,
        actor: CreditRedemptionActor,
        input: CreateCreditRedemptionCodesInput,
    ) -> Result<CreateCreditRedemptionCodesPayload, BillingError>;
    async fn revoke_credit_redemption_code(
        &self,
        actor: CreditRedemptionActor,
        code_id: &str,
    ) -> Result<CreditRedemptionCode, BillingError>;
    async fn redeem_credit_code(
        &self,
        actor: CreditRedemptionActor,
        user_id: &str,
        project_id: &str,
        code: &str,
    ) -> Result<CreditRedemptionReceipt, BillingError>;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redemption_amount_matches_exact_micros_contract() {
        for valid in ["1", "0.000001", "12.340000", " 7.5 "] {
            assert!(
                validate_credit_redemption_amount(valid).is_ok(),
                "{valid:?} must be valid"
            );
        }
        for invalid in [
            "",
            "0",
            "-1",
            "+1",
            ".5",
            "1.",
            "1.0000000",
            "1e2",
            "1_000",
            "9223372036854.775808",
        ] {
            assert!(
                validate_credit_redemption_amount(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
    }

    #[test]
    fn redemption_creation_bounds_quantity_description_and_expiry() {
        let mut input = CreateCreditRedemptionCodesInput {
            amount: "10.25".into(),
            quantity: MAX_CREDIT_REDEMPTION_QUANTITY,
            max_redemptions: DEFAULT_CREDIT_REDEMPTIONS_PER_CODE,
            expires_at: Some("2999-01-01T00:00:00Z".into()),
            description: Some("campaign".into()),
        };
        assert!(validate_create_credit_redemption_codes_input(&input).is_ok());

        input.quantity = MAX_CREDIT_REDEMPTION_QUANTITY + 1;
        assert!(validate_create_credit_redemption_codes_input(&input).is_err());
        input.quantity = 1;
        input.max_redemptions = MAX_CREDIT_REDEMPTIONS_PER_CODE;
        assert!(validate_create_credit_redemption_codes_input(&input).is_ok());
        input.max_redemptions = 0;
        assert!(validate_create_credit_redemption_codes_input(&input).is_err());
        input.max_redemptions = MAX_CREDIT_REDEMPTIONS_PER_CODE + 1;
        assert!(validate_create_credit_redemption_codes_input(&input).is_err());
        input.max_redemptions = DEFAULT_CREDIT_REDEMPTIONS_PER_CODE;
        input.description = Some("x".repeat(MAX_CREDIT_REDEMPTION_DESCRIPTION_CHARS + 1));
        assert!(validate_create_credit_redemption_codes_input(&input).is_err());
        input.description = None;
        input.expires_at = Some("2020-01-01T00:00:00Z".into());
        assert!(validate_create_credit_redemption_codes_input(&input).is_err());
    }

    #[test]
    fn redemption_code_normalization_is_bounded_and_uses_one_public_error() {
        assert_eq!(
            normalize_credit_redemption_code("  cr-ab12-cd34  ").expect("valid normalized code"),
            "CR-AB12-CD34"
        );
        for invalid in [
            "short",
            "contains space",
            "兑换码-12345678",
            "A/B/C/12345678",
        ] {
            assert!(matches!(
                normalize_credit_redemption_code(invalid),
                Err(BillingError::RedemptionCodeUnavailable)
            ));
        }
    }

    #[test]
    fn redemption_pagination_is_strictly_bounded() {
        assert!(validate_credit_redemption_pagination(1, 0).is_ok());
        assert!(
            validate_credit_redemption_pagination(MAX_CREDIT_REDEMPTION_PAGE_LIMIT, 10).is_ok()
        );
        assert!(validate_credit_redemption_pagination(0, 0).is_err());
        assert!(
            validate_credit_redemption_pagination(MAX_CREDIT_REDEMPTION_PAGE_LIMIT + 1, 0).is_err()
        );
        assert!(validate_credit_redemption_pagination(1, -1).is_err());
    }
}

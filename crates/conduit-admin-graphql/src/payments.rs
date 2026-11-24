//! Project-wallet topups. All money fields use decimal strings on the wire.
use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};
use std::sync::Arc;

#[derive(Debug, Clone, SimpleObject)]
pub struct PaymentCapabilities {
    pub enabled: bool,
    pub currency: String,
    pub channels: Vec<PaymentChannel>,
    pub min_amount_minor: String,
    pub max_amount_minor: String,
}
pub fn services(ctx: &Context<'_>) -> Result<Arc<dyn PaymentServices>, String> {
    ctx.data::<Arc<dyn PaymentServices>>()
        .cloned()
        .map_err(|_| "payment services unavailable".into())
}
pub fn identity(ctx: &Context<'_>) -> Result<(i64, i64), String> {
    let current = ctx
        .data::<crate::me::CurrentUser>()
        .map_err(|_| "authentication required")?;
    let project = crate::policy::request_context(ctx)
        .and_then(|r| r.project_id.as_deref())
        .ok_or("current Project required")?;
    Ok((current.user_id, crate::groups::numeric_id(project)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PaymentChannel {
    Alipay,
    Wechat,
}
impl PaymentChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alipay => "alipay",
            Self::Wechat => "wechat",
        }
    }
}
#[derive(Debug, Clone, SimpleObject)]
pub struct PaymentOrder {
    pub id: ID,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub status: String,
    pub channel: PaymentChannel,
    pub currency: String,
    pub amount_minor: String,
    pub credits_micros: String,
    #[graphql(name = "rechargeMultiplierPPM")]
    pub recharge_multiplier_ppm: String,
    pub accounting_version: String,
    pub created_at: String,
    pub expires_at: String,
    pub paid_at: Option<String>,
    #[graphql(name = "checkoutURL")]
    pub checkout_url: Option<String>,
    pub review_reason: Option<String>,
}
#[derive(Debug, Clone, InputObject)]
pub struct CreatePaymentOrderInput {
    /// Positive integer CNY fen, with no exponent, sign or fractional part.
    pub amount_minor: String,
    pub channel: PaymentChannel,
    pub idempotency_key: String,
}

/// Global billing authority, independent of the selected Project. Private
/// fields prevent services being called with a fabricated operator permit.
pub struct OperatorPaymentAccess {
    write: bool,
}
impl OperatorPaymentAccess {
    pub fn from_request(
        request: &conduit_auth::request_context::RequestContext,
        write: bool,
    ) -> Result<Self, String> {
        let scope = if write {
            conduit_auth::scopes::slug::WRITE_BILLING
        } else {
            conduit_auth::scopes::slug::READ_BILLING
        };
        let decision = conduit_auth::rbac::has_scope(request, scope);
        if !matches!(
            decision.source(),
            Some(
                conduit_auth::rbac::PermissionSource::Owner
                    | conduit_auth::rbac::PermissionSource::DirectScope
                    | conduit_auth::rbac::PermissionSource::SystemRoleScope
                    | conduit_auth::rbac::PermissionSource::System
                    | conduit_auth::rbac::PermissionSource::Test
            )
        ) {
            return Err(format!("global {scope} required"));
        }
        Ok(Self { write })
    }
    pub fn require_write(&self) -> Result<(), String> {
        if self.write {
            Ok(())
        } else {
            Err("global write_billing required".into())
        }
    }
}
pub fn operator_access(ctx: &Context<'_>, write: bool) -> Result<OperatorPaymentAccess, String> {
    OperatorPaymentAccess::from_request(
        crate::policy::request_context(ctx).ok_or("authentication required")?,
        write,
    )
}
#[derive(Debug, Clone, SimpleObject)]
pub struct OperatorPaymentOrder {
    #[graphql(name = "payerUserID")]
    pub payer_user_id: ID,
    pub order: PaymentOrder,
}
#[async_trait::async_trait]
pub trait PaymentServices: Send + Sync {
    async fn operator_orders(
        &self,
        access: OperatorPaymentAccess,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<OperatorPaymentOrder>, String>;
    async fn operator_order(
        &self,
        access: OperatorPaymentAccess,
        id: &str,
    ) -> Result<OperatorPaymentOrder, String>;
    async fn operator_recheck(
        &self,
        access: OperatorPaymentAccess,
        id: &str,
    ) -> Result<OperatorPaymentOrder, String>;
    fn capabilities(&self) -> PaymentCapabilities;
    async fn orders(
        &self,
        payer: i64,
        project: i64,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<PaymentOrder>, String>;
    async fn create(
        &self,
        payer: i64,
        project: i64,
        input: CreatePaymentOrderInput,
    ) -> Result<PaymentOrder, String>;
    async fn order(&self, payer: i64, project: i64, id: &str) -> Result<PaymentOrder, String>;
    /// Performs a provider query; never accepts a client-supplied paid flag.
    async fn recheck(&self, payer: i64, project: i64, id: &str) -> Result<PaymentOrder, String>;
}

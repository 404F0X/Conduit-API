//! Operation-scoped commercial changes. Connections deliberately exclude snapshot bodies.
use crate::{
    change_set::{
        ChangeSetAction, ChangeSetError, ChangeSetEvent, ChangeSetItem, ChangeSetKind,
        ChangeSetStatus,
    },
    pagination::PageInfo,
    scalars::{CursorScalar, TimeScalar},
};
use async_graphql::{Context, Enum, ErrorExtensions, ID, InputObject, Json, SimpleObject};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum OperationOrigin {
    ChannelCreate,
    UserSync,
    AutomaticSync,
    ManualEdit,
    ChannelImport,
    Correction,
    Unknown,
}
impl OperationOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChannelCreate => "CHANNEL_CREATE",
            Self::UserSync => "USER_SYNC",
            Self::AutomaticSync => "AUTOMATIC_SYNC",
            Self::ManualEdit => "MANUAL_EDIT",
            Self::ChannelImport => "CHANNEL_IMPORT",
            Self::Correction => "CORRECTION",
            Self::Unknown => "UNKNOWN",
        }
    }
    pub fn from_stored(s: &str) -> Self {
        match s {
            "CHANNEL_CREATE" => Self::ChannelCreate,
            "USER_SYNC" => Self::UserSync,
            "AUTOMATIC_SYNC" => Self::AutomaticSync,
            "MANUAL_EDIT" => Self::ManualEdit,
            "CHANNEL_IMPORT" => Self::ChannelImport,
            "CORRECTION" => Self::Correction,
            _ => Self::Unknown,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ValidationState {
    Unchecked,
    Valid,
    Invalid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum IssueSeverity {
    Blocking,
    Warning,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetIssue {
    pub item_key: Option<String>,
    pub field: Option<String>,
    pub code: String,
    pub message: String,
    pub severity: IssueSeverity,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChangeSetAvailableAction {
    Edit,
    Submit,
    ApproveApply,
    Approve,
    Publish,
    Reject,
    Recreate,
}
#[derive(Debug, Clone, InputObject, Default)]
pub struct ChangeSetFilterInput {
    pub kind: Option<ChangeSetKind>,
    pub statuses: Option<Vec<ChangeSetStatus>>,
    pub scope_type: Option<String>,
    #[graphql(name = "scopeID")]
    pub scope_id: Option<ID>,
    pub search: Option<String>,
    #[graphql(skip)]
    pub allowed_kinds: Option<Vec<ChangeSetKind>>,
    #[graphql(skip)]
    pub actor_user_id: Option<i64>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetStatusCount {
    pub status: ChangeSetStatus,
    pub count: i32,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetSummary {
    pub id: ID,
    #[graphql(name = "operationID")]
    pub operation_id: Option<ID>,
    pub kind: ChangeSetKind,
    pub scope_type: String,
    #[graphql(name = "scopeID")]
    pub scope_id: String,
    pub scope_name: Option<String>,
    pub title: String,
    pub status: ChangeSetStatus,
    pub revision: String,
    pub source_origin: OperationOrigin,
    pub created_by: Option<ID>,
    pub actor_name: Option<String>,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub submitted_at: Option<TimeScalar>,
    pub reviewed_at: Option<TimeScalar>,
    pub applied_at: Option<TimeScalar>,
    pub item_count: i32,
    pub create_count: i32,
    pub update_count: i32,
    pub delete_count: i32,
    pub invalid_item_count: i32,
    pub validation_state: ValidationState,
    pub validation_error: Option<String>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetSummaryEdge {
    pub cursor: CursorScalar,
    pub node: ChangeSetSummary,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetSummaryConnection {
    pub edges: Vec<ChangeSetSummaryEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
    pub status_counts: Vec<ChangeSetStatusCount>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetAppliedTarget {
    pub r#type: String,
    pub id: ID,
    pub item_key: String,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetDetail {
    pub summary: ChangeSetSummary,
    pub base_revision: String,
    pub source_revision: String,
    pub current_base_revision: Option<String>,
    pub submitted_by: Option<ID>,
    pub reviewed_by: Option<ID>,
    pub review_note: Option<String>,
    pub issues: Vec<ChangeSetIssue>,
    pub available_actions: Vec<ChangeSetAvailableAction>,
    pub applied_targets: Vec<ChangeSetAppliedTarget>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetItemSummary {
    pub id: ID,
    pub item_key: String,
    pub action: ChangeSetAction,
    pub model_label: Option<String>,
    pub changed_field_count: i32,
    pub validation_state: ValidationState,
    pub issues: Vec<ChangeSetIssue>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetItemDetail {
    pub item: ChangeSetItem,
    pub current_snapshot: Option<Json<Value>>,
    pub current_matches_before: bool,
    pub issues: Vec<ChangeSetIssue>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetItemSummaryEdge {
    pub cursor: CursorScalar,
    pub node: ChangeSetItemSummary,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetItemSummaryConnection {
    pub edges: Vec<ChangeSetItemSummaryEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetEventEdge {
    pub cursor: CursorScalar,
    pub node: ChangeSetEvent,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetEventConnection {
    pub edges: Vec<ChangeSetEventEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}
#[derive(Debug, Clone, InputObject)]
pub struct ChangeSetCorrectionInput {
    #[graphql(name = "sourceChangeSetID")]
    pub source_change_set_id: ID,
    pub expected_source_package_revision: String,
    pub excluded_item_keys: Vec<String>,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum CreateChangeSetOutcome {
    Created,
    NoChange,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct CreateChangeSetResult {
    #[graphql(name = "operationID")]
    pub operation_id: ID,
    pub outcome: CreateChangeSetOutcome,
    pub change_set_detail: Option<ChangeSetDetail>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct SaveRetailPriceChangeSetItemResult {
    pub item: ChangeSetItem,
    pub change_set_revision: String,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ProviderPriceEditBase {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub revision: String,
    pub accounting_currency: String,
    pub accounting_settings_version: String,
    pub prices: Vec<crate::model_ext::ChannelModelPrice>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct RetailPriceEditBaseItem {
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    pub price: Json<Value>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct RetailPriceEditBase {
    #[graphql(name = "priceBookID")]
    pub price_book_id: ID,
    pub revision: String,
    pub accounting_currency: String,
    pub accounting_settings_version: String,
    pub prices: Vec<RetailPriceEditBaseItem>,
}

/// The caller's resolved system permissions are hints for UI actions; root authorization remains mandatory.
#[derive(Debug, Clone, Copy)]
pub struct PackageViewer {
    pub actor_user_id: i64,
    pub can_write: bool,
}
#[async_trait]
pub trait ChangePackageServices: Send + Sync {
    async fn list_summaries(
        &self,
        filter: ChangeSetFilterInput,
        after: Option<CursorScalar>,
        first: i32,
    ) -> Result<ChangeSetSummaryConnection, ChangeSetError>;
    async fn get_detail(
        &self,
        viewer: PackageViewer,
        id: ID,
    ) -> Result<Option<ChangeSetDetail>, ChangeSetError>;
    async fn list_items(
        &self,
        id: ID,
        after: Option<CursorScalar>,
        first: i32,
        search: Option<String>,
        problems_only: bool,
    ) -> Result<ChangeSetItemSummaryConnection, ChangeSetError>;
    async fn get_item(
        &self,
        id: ID,
        item_id: ID,
    ) -> Result<Option<ChangeSetItemDetail>, ChangeSetError>;
    async fn list_events(
        &self,
        id: ID,
        after: Option<CursorScalar>,
        first: i32,
    ) -> Result<ChangeSetEventConnection, ChangeSetError>;
    async fn find_by_operation(
        &self,
        viewer: PackageViewer,
        operation_id: ID,
        kind: ChangeSetKind,
        scope_type: String,
        scope_id: ID,
    ) -> Result<Option<ChangeSetDetail>, ChangeSetError>;
    async fn find_provider_operation(
        &self,
        viewer: PackageViewer,
        operation_id: ID,
    ) -> Result<Option<CreateChangeSetResult>, ChangeSetError>;
    async fn provider_edit_base(&self, id: ID) -> Result<ProviderPriceEditBase, ChangeSetError>;
    async fn retail_edit_base(&self, id: ID) -> Result<RetailPriceEditBase, ChangeSetError>;
    async fn create_provider(
        &self,
        actor: i64,
        channel: ID,
        operation: ID,
        expected_base: String,
        input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
        correction: Option<ChangeSetCorrectionInput>,
    ) -> Result<CreateChangeSetResult, ChangeSetError>;
    async fn save_provider(
        &self,
        actor: i64,
        id: ID,
        revision: String,
        input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<ChangeSetDetail, ChangeSetError>;
    async fn create_retail(
        &self,
        actor: i64,
        book: ID,
        operation: ID,
        expected_base: String,
    ) -> Result<ChangeSetDetail, ChangeSetError>;
    async fn save_retail_item(
        &self,
        actor: i64,
        input: crate::change_set::SaveRetailPriceChangeSetItemInput,
    ) -> Result<SaveRetailPriceChangeSetItemResult, ChangeSetError>;
    async fn transition(
        &self,
        actor: i64,
        id: ID,
        revision: String,
        action: ChangeSetAvailableAction,
        note: Option<String>,
    ) -> Result<ChangeSetDetail, ChangeSetError>;
}
pub(crate) fn services(ctx: &Context<'_>) -> async_graphql::Result<Arc<dyn ChangePackageServices>> {
    ctx.data::<Arc<dyn ChangePackageServices>>()
        .cloned()
        .map_err(|_| async_graphql::Error::new("commercial change services unavailable"))
}
pub(crate) fn viewer(ctx: &Context<'_>) -> async_graphql::Result<PackageViewer> {
    let actor = ctx
        .data::<crate::me::CurrentUser>()
        .map_err(|_| async_graphql::Error::new("authentication required"))?
        .user_id;
    let auth = ctx
        .data_opt::<conduit_auth::RequestContext>()
        .and_then(|r| r.principal.as_ref());
    let can_write = auth.is_some();
    Ok(PackageViewer {
        actor_user_id: actor,
        can_write,
    })
}
pub(crate) async fn authorize_read(ctx: &Context<'_>, id: ID) -> async_graphql::Result<()> {
    if let Some(service) = ctx.data_opt::<Arc<dyn crate::governance::GovernanceServices>>() {
        service
            .authorize_read(
                crate::governance::actor(ctx).map_err(async_graphql::Error::new)?,
                id,
            )
            .await
            .map_err(async_graphql::Error::new)?;
    }
    Ok(())
}
pub fn graphql_error(error: ChangeSetError) -> async_graphql::Error {
    let message = error.to_string();
    let code = message
        .split(':')
        .find(|s| {
            [
                "REVISION_CONFLICT",
                "STATE_CHANGED",
                "VALIDATION_FAILED",
                "SOURCE_CHANGED",
                "OPERATION_CONFLICT",
                "BAD_CURSOR",
                "STORAGE_FAILED",
            ]
            .contains(&s.trim())
        })
        .map(str::trim)
        .unwrap_or(match error {
            ChangeSetError::Storage(_) => "STORAGE_FAILED",
            _ => "VALIDATION_FAILED",
        });
    async_graphql::Error::new(message.clone()).extend_with(|_, ext| ext.set("code", code))
}

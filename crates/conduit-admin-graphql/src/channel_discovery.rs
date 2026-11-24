//! Durable, stage-specific NEWAPI discovery. Secrets are write-only inputs.
use crate::change_package::OperationOrigin;
use crate::scalars::TimeScalar;
use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChannelDiscoveryStage {
    Models,
    Prices,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChannelDiscoveryStageStatus {
    NotStarted,
    Running,
    Succeeded,
    NoChange,
    Failed,
    Blocked,
    Interrupted,
    Skipped,
}
impl ChannelDiscoveryStageStatus {
    pub fn complete(self) -> bool {
        matches!(self, Self::Succeeded | Self::NoChange | Self::Skipped)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelDiscoveryState {
    NotStarted,
    Running,
    Succeeded,
    Partial,
    NeedsAction,
    Failed,
    Interrupted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PackageValidationStatus {
    Unchecked,
    Valid,
    NeedsAction,
    NoChange,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RecurringPriceDiscoveryState {
    Disabled,
    AwaitingQuotaVerification,
    Enabled,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelDiscoveryStageResult {
    pub status: ChannelDiscoveryStageStatus,
    pub discovered_count: i32,
    pub changed_count: i32,
    pub unsupported_count: i32,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub retryable: bool,
    #[graphql(name = "snapshotID")]
    pub snapshot_id: Option<ID>,
    pub completed_at: Option<TimeScalar>,
}
impl ChannelDiscoveryStageResult {
    pub fn initial(requested: bool) -> Self {
        Self {
            status: if requested {
                ChannelDiscoveryStageStatus::NotStarted
            } else {
                ChannelDiscoveryStageStatus::Skipped
            },
            discovered_count: 0,
            changed_count: 0,
            unsupported_count: 0,
            error_code: None,
            message: None,
            retryable: false,
            snapshot_id: None,
            completed_at: None,
        }
    }
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelDiscoveryPackageValidation {
    pub status: PackageValidationStatus,
    pub total_items: i32,
    pub valid_items: i32,
    pub invalid_items: i32,
    pub warning_items: i32,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelDiscoveryPackage {
    #[graphql(name = "changeSetID")]
    pub change_set_id: ID,
    pub kind: crate::change_set::ChangeSetKind,
    pub status: crate::change_set::ChangeSetStatus,
    pub item_count: i32,
    pub reused: bool,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelDiscoveryOperation {
    #[graphql(name = "operationID")]
    pub operation_id: ID,
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub origin: OperationOrigin,
    pub created_by: Option<ID>,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub attempt: i32,
    pub state: ChannelDiscoveryState,
    pub channel_created: bool,
    pub models: ChannelDiscoveryStageResult,
    pub prices: ChannelDiscoveryStageResult,
    pub package_validation: ChannelDiscoveryPackageValidation,
    pub packages: Vec<ChannelDiscoveryPackage>,
    pub lease_expires_at: Option<TimeScalar>,
    pub recurring_model_sync_enabled: bool,
    pub recurring_price_discovery_state: RecurringPriceDiscoveryState,
    #[graphql(name = "supersededSourceChangeSetIDs")]
    pub superseded_source_change_set_ids: Vec<ID>,
}
#[derive(Debug, Clone, InputObject, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSetRevisionInput {
    pub id: ID,
    pub expected_revision: String,
}
#[derive(Debug, Clone, InputObject, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildObservedPricePackagesInput {
    pub sources: Vec<ChangeSetRevisionInput>,
    pub supersede_sources: bool,
}
#[derive(Debug, Clone, InputObject)]
pub struct NewApiAccountCredentialsInput {
    pub pat: String,
    #[graphql(name = "userID")]
    pub user_id: Option<String>,
}
#[async_trait]
pub trait ChannelDiscoveryServices: Send + Sync {
    async fn get(&self, operation: ID) -> Result<Option<ChannelDiscoveryOperation>, String>;
    async fn initialize(
        &self,
        actor: i64,
        can_rebuild: bool,
        channel: ID,
        operation: ID,
        stages: Vec<ChannelDiscoveryStage>,
        rebuild: Option<RebuildObservedPricePackagesInput>,
    ) -> Result<ChannelDiscoveryOperation, String>;
    async fn retry(
        &self,
        actor: i64,
        can_rebuild: bool,
        operation: ID,
        attempt: i32,
        stages: Vec<ChannelDiscoveryStage>,
        credentials: Option<NewApiAccountCredentialsInput>,
    ) -> Result<ChannelDiscoveryOperation, String>;
}
pub(crate) fn services(
    ctx: &Context<'_>,
) -> async_graphql::Result<Arc<dyn ChannelDiscoveryServices>> {
    ctx.data::<Arc<dyn ChannelDiscoveryServices>>()
        .cloned()
        .map_err(|_| async_graphql::Error::new("channel discovery service unavailable"))
}

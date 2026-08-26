use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, Json, SimpleObject};
use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::scalars::TimeScalar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChangeSetKind {
    ProviderPrice,
    ModelMapping,
    RetailPrice,
}

impl ChangeSetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderPrice => "provider_price",
            Self::ModelMapping => "model_mapping",
            Self::RetailPrice => "retail_price",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChangeSetStatus {
    Draft,
    PendingReview,
    Applied,
    Rejected,
    Superseded,
    Invalid,
}

impl ChangeSetStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingReview => "pending_review",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChangeSetAction {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetItem {
    pub id: ID,
    pub item_key: String,
    pub action: ChangeSetAction,
    pub before_snapshot: Option<Json<Value>>,
    pub after_snapshot: Option<Json<Value>>,
    pub source_snapshot: Option<Json<Value>>,
    pub validation_error: Option<String>,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSetEvent {
    pub id: ID,
    pub event_type: String,
    pub actor_type: String,
    #[graphql(name = "actorID")]
    pub actor_id: Option<ID>,
    pub detail: Json<Value>,
    pub created_at: TimeScalar,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeSet {
    pub id: ID,
    pub kind: ChangeSetKind,
    pub scope_type: String,
    #[graphql(name = "scopeID")]
    pub scope_id: String,
    pub title: String,
    pub status: ChangeSetStatus,
    pub base_revision: String,
    pub source_revision: String,
    pub applied_target_type: Option<String>,
    #[graphql(name = "appliedTargetID")]
    pub applied_target_id: Option<String>,
    pub validation_error: Option<String>,
    #[graphql(name = "createdBy")]
    pub created_by: Option<ID>,
    #[graphql(name = "submittedBy")]
    pub submitted_by: Option<ID>,
    #[graphql(name = "reviewedBy")]
    pub reviewed_by: Option<ID>,
    pub review_note: Option<String>,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub submitted_at: Option<TimeScalar>,
    pub reviewed_at: Option<TimeScalar>,
    pub applied_at: Option<TimeScalar>,
    pub items: Vec<ChangeSetItem>,
    pub events: Vec<ChangeSetEvent>,
}

#[derive(Debug, Clone, InputObject)]
pub struct SaveRetailPriceChangeSetItemInput {
    #[graphql(name = "changeSetID")]
    pub change_set_id: ID,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    pub price: Json<Value>,
}

#[derive(Debug, Error)]
pub enum ChangeSetError {
    #[error("change set query failed: {0}")]
    Query(String),
    #[error("invalid change set operation: {0}")]
    Invalid(String),
    #[error("change set operation failed: {0}")]
    Storage(String),
}

#[async_trait]
pub trait ChangeSetServices: Send + Sync {
    async fn change_sets(
        &self,
        kind: Option<ChangeSetKind>,
        status: Option<ChangeSetStatus>,
        scope_type: Option<String>,
        scope_id: Option<String>,
        limit: i32,
    ) -> Result<Vec<ChangeSet>, ChangeSetError>;

    async fn create_provider_price_change_set(
        &self,
        actor_user_id: i64,
        channel_id: ID,
        input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<ChangeSet, ChangeSetError>;

    async fn create_retail_price_change_set(
        &self,
        actor_user_id: i64,
        price_book_id: ID,
    ) -> Result<ChangeSet, ChangeSetError>;

    async fn save_retail_price_change_set_item(
        &self,
        actor_user_id: i64,
        input: SaveRetailPriceChangeSetItemInput,
    ) -> Result<ChangeSetItem, ChangeSetError>;

    async fn submit_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
    ) -> Result<ChangeSet, ChangeSetError>;

    async fn approve_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
        review_note: Option<String>,
    ) -> Result<ChangeSet, ChangeSetError>;

    async fn reject_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
        review_note: Option<String>,
    ) -> Result<ChangeSet, ChangeSetError>;
}

pub(crate) fn change_set_services(ctx: &Context<'_>) -> Result<Arc<dyn ChangeSetServices>, String> {
    ctx.data::<Arc<dyn ChangeSetServices>>()
        .cloned()
        .map_err(|_| "change set services unavailable".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_exposes_only_the_unified_change_set_workflow() {
        let sdl = crate::admin_schema_builder().finish().sdl();
        for field in [
            "changeSets(",
            "createProviderPriceChangeSet(",
            "createRetailPriceChangeSet(",
            "saveRetailPriceChangeSetItem(",
            "submitChangeSet(",
            "approveChangeSet(",
            "rejectChangeSet(",
        ] {
            assert!(sdl.contains(field), "missing GraphQL field {field}");
        }
        for removed in [
            "providerPriceDrafts(",
            "approveProviderPriceDraft(",
            "createPriceBookDraft(",
            "publishPriceBookVersion(",
        ] {
            assert!(!sdl.contains(removed), "obsolete GraphQL field {removed}");
        }
        assert!(sdl.contains("scopeID: String!"));
        assert!(sdl.contains("actorID: ID"));
        assert!(sdl.contains("PENDING_REVIEW"));
    }
}

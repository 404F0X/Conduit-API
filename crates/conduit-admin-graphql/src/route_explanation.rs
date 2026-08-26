use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

use crate::scalars::JsonRawMessageScalar;

#[derive(Debug, Clone, SimpleObject)]
pub struct RouteRetryReason {
    pub sequence: i32,
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    pub status: String,
    pub reason: Option<String>,
    pub response_status_code: Option<i64>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct RequestRouteExplanation {
    #[graphql(name = "requestID")]
    pub request_id: ID,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub requested_model: String,
    pub load_balance_strategy: String,
    pub selected_candidates: JsonRawMessageScalar,
    pub rejected_candidates: JsonRawMessageScalar,
    pub ordered_candidates: JsonRawMessageScalar,
    #[graphql(name = "finalChannelID")]
    pub final_channel_id: Option<ID>,
    pub final_model_id: Option<String>,
    pub terminal_error: Option<String>,
    pub affinity_key_class: Option<String>,
    pub affinity_decision: Option<String>,
    pub retry_reasons: Vec<RouteRetryReason>,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteExplanationError {
    #[error("route explanation service is unavailable")]
    Unavailable,
    #[error("route explanation query failed: {0}")]
    Query(String),
}

#[async_trait::async_trait]
pub trait RouteExplanationServices: Send + Sync {
    async fn request_route_explanation(
        &self,
        request_id: &str,
        project_id: Option<&str>,
    ) -> Result<Option<RequestRouteExplanation>, RouteExplanationError>;
}

pub(crate) fn route_explanation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn RouteExplanationServices>, String> {
    ctx.data::<Arc<dyn RouteExplanationServices>>()
        .cloned()
        .map_err(|_| RouteExplanationError::Unavailable.to_string())
}

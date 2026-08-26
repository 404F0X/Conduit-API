use async_graphql::ID;
use async_trait::async_trait;
use conduit_admin_graphql::route_explanation::{
    RequestRouteExplanation, RouteExplanationError, RouteExplanationServices, RouteRetryReason,
};
use conduit_admin_graphql::scalars::JsonRawMessageScalar;
use serde_json::Value;

pub struct RouteExplanationAdapter {
    pool: sqlx::PgPool,
}

impl RouteExplanationAdapter {
    pub fn postgres(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RouteExplanationServices for RouteExplanationAdapter {
    async fn request_route_explanation(
        &self,
        request_id: &str,
        project_id: Option<&str>,
    ) -> Result<Option<RequestRouteExplanation>, RouteExplanationError> {
        let request_id_num = request_id.parse::<i64>().map_err(|_| {
            RouteExplanationError::Query("request ID is not a valid integer".to_owned())
        })?;
        load_postgres(&self.pool, request_id_num, project_id).await
    }
}
async fn load_postgres(
    pool: &sqlx::PgPool,
    request_id: i64,
    project_id: Option<&str>,
) -> Result<Option<RequestRouteExplanation>, RouteExplanationError> {
    let project_id = project_id.and_then(|value| value.parse::<i64>().ok());
    let row = sqlx::query_as::<_, (i64, i64, String, String, Value, Value, Value, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>)>(
        "SELECT request_id,project_id,requested_model,load_balance_strategy,selected_candidates,rejected_candidates,ordered_candidates,final_channel_id,final_model_id,terminal_error,affinity_key_class,affinity_decision FROM request_route_explanations WHERE request_id=$1 AND ($2::bigint IS NULL OR project_id=$2)",
    )
    .bind(request_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| RouteExplanationError::Query(error.to_string()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let retries = sqlx::query_as::<_, (i64, Option<i64>, String, Option<String>, Option<i64>)>(
        "SELECT ROW_NUMBER() OVER (ORDER BY created_at,id),channel_id,status,error_message,response_status_code FROM request_executions WHERE request_id=$1 AND status IN ('failed','canceled') ORDER BY created_at,id",
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RouteExplanationError::Query(error.to_string()))?;
    Ok(Some(build(
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10, row.11,
        retries,
    )))
}

fn build(
    request_id: i64,
    project_id: i64,
    requested_model: String,
    strategy: String,
    selected: Value,
    rejected: Value,
    ordered: Value,
    final_channel_id: Option<i64>,
    final_model_id: Option<String>,
    terminal_error: Option<String>,
    affinity_key_class: Option<String>,
    affinity_decision: Option<String>,
    retries: Vec<(i64, Option<i64>, String, Option<String>, Option<i64>)>,
) -> RequestRouteExplanation {
    RequestRouteExplanation {
        request_id: ID::from(request_id.to_string()),
        project_id: ID::from(project_id.to_string()),
        requested_model,
        load_balance_strategy: strategy,
        selected_candidates: JsonRawMessageScalar(selected),
        rejected_candidates: JsonRawMessageScalar(rejected),
        ordered_candidates: JsonRawMessageScalar(ordered),
        final_channel_id: final_channel_id.map(|id| ID::from(id.to_string())),
        final_model_id,
        terminal_error,
        affinity_key_class,
        affinity_decision,
        retry_reasons: retries
            .into_iter()
            .map(|row| RouteRetryReason {
                sequence: i32::try_from(row.0).unwrap_or(i32::MAX),
                channel_id: row.1.map(|id| ID::from(id.to_string())),
                status: row.2,
                reason: row.3,
                response_status_code: row.4,
            })
            .collect(),
    }
}

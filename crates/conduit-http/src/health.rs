use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

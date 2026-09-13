use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[async_trait]
pub trait ReadinessService: Send + Sync {
    async fn check(&self) -> Result<(), String>;
}

#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub status: &'static str,
    pub database: &'static str,
}

pub async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<ReadinessResponse>) {
    let ready = match state.services().readiness_service() {
        Some(service) => service.check().await.is_ok(),
        None => false,
    };
    if ready {
        (
            StatusCode::OK,
            Json(ReadinessResponse {
                status: "ready",
                database: "ok",
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                status: "not_ready",
                database: "unavailable",
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::AppServices;

    struct StubReadiness(bool);

    #[async_trait]
    impl ReadinessService for StubReadiness {
        async fn check(&self) -> Result<(), String> {
            self.0.then_some(()).ok_or_else(|| "not ready".to_string())
        }
    }

    fn state(ready: bool) -> AppState {
        AppState::new(
            Arc::new(conduit_config::AppConfig::default()),
            Arc::new(AppServices::new().with_readiness_service(Arc::new(StubReadiness(ready)))),
        )
    }

    #[tokio::test]
    async fn readiness_reports_database_state_without_details() {
        let (status, Json(body)) = readiness(State(state(true))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.status, "ready");
        assert_eq!(body.database, "ok");

        let (status, Json(body)) = readiness(State(state(false))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.status, "not_ready");
        assert_eq!(body.database, "unavailable");
    }
}

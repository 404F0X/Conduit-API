//! Classic reference profile notifications are GET-only. Browser return is UI-only.
use crate::app_state::AppState;
use axum::{
    extract::{RawQuery, State},
    http::StatusCode,
};

#[async_trait::async_trait]
pub trait PaymentNotificationService: Send + Sync {
    /// Returns only after a verified outcome is durable; errors request retry.
    async fn notify(&self, raw_query: &str) -> Result<(), String>;
}
pub async fn epay_notify(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> (StatusCode, &'static str) {
    let Some(service) = state.services().payment_notification_service() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "fail");
    };
    match service.notify(query.as_deref().unwrap_or_default()).await {
        Ok(()) => (StatusCode::OK, "success"),
        Err(_) => (StatusCode::BAD_GATEWAY, "fail"),
    }
}

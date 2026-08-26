//! ADPT-VIDEO — host adapter wiring [`conduit_http::VideoService`] to the
//! database-independent request repository.
//!
//! Replaces the stub `StubVideoService` that returned "not available" for all
//! video task operations.
//!
//! ## Go parity anchors (`conduit/internal/server/biz/video.go`)
//!
//! - `GetTaskByExternalID` (lines 59-75): queries the `requests` table by
//!   `external_id` (globally unique across channels), loads the request row,
//!   then calls the provider's `GetTask` endpoint via the video outbound
//!   transformer to get the current task state. The Rust adapter loads the
//!   request row and returns the stored `response_body` as the wire payload.
//!   The provider-side `GetTask` HTTP call is DEFER'd until the video
//!   outbound transformer pipeline is wired.
//!
//! - `DeleteTaskByExternalID` (lines 77-93): queries by `external_id`, deletes
//!   the task via the provider's `DeleteVideoTask` endpoint, then best-effort
//!   marks the local request row as `canceled`. The Rust adapter performs
//!   only the local soft-cancel (status → `canceled`). The upstream provider
//!   HTTP call is DEFER'd.
//!
//! ## Design
//!
//! Go's `VideoService.GetTaskByExternalID` runs `client.Request.Query().
//! Where(external_id)`, which looks project-agnostic but is NOT: the ent
//! `Request` privacy policy injects the caller's project scope before the SQL
//! executes (`schema/request.go:173-189`). The Rust adapter explicitly passes
//! the caller's `project_id` into the shared request repository (P-23).
//! A foreign external id therefore resolves to "not found" instead of leaking
//! another project's task (the raw-SQL bridge previously dropped the filter,
//! which is the cross-project IDOR P-23 closes).

use std::sync::Arc;

use async_trait::async_trait;

use conduit_core::error::ConduitError;
use conduit_db::repo::request_repo::{RequestRepo, STATUS_CANCELED};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_http::openai_handlers::OpenAiHandlerResponse;

/// Adapter implementing [`conduit_http::VideoService`] backed by the live
/// shared [`RequestRepo`]. Keeping SQL in the repository preserves the video-task behavior and
/// project-isolation rules.
///
/// Mirrors Go's `biz.VideoService` (video.go) with two bounded-scope
/// concessions documented as DEFER'd:
///
/// * `get_task_by_external_id` returns the stored `response_body` from the
///   local request row instead of round-tripping through the provider's
///   GetTask endpoint + video outbound transformer.
/// * `delete_task_by_external_id` marks the local request row as `canceled`
///   but does NOT issue the upstream provider delete HTTP call.
pub struct VideoAdapter {
    request_repo: Arc<dyn RequestRepo>,
}

impl VideoAdapter {
    /// Build the adapter over the runtime's request repository.
    pub fn new(request_repo: Arc<dyn RequestRepo>) -> Self {
        Self { request_repo }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::system()))
    }
}

#[async_trait]
impl conduit_http::VideoService for VideoAdapter {
    /// Mirrors Go `VideoService.GetTaskByExternalID` (video.go:59-75).
    ///
    /// Loads the request row by `external_id` (globally unique across
    /// channels — Go's NOTE at video.go:60), extracts the stored
    /// `response_body`, and wraps it in an [`OpenAiHandlerResponse`].
    ///
    /// DEFER: the provider-side `GetTask` HTTP call + video outbound
    /// transformer (`BuildGetVideoTaskRequest` + `ParseGetVideoTaskResponse`
    /// + `UpdateRequestStatusExternalIDAndResponseBody`) is not wired. The
    /// stored snapshot from the most recent orchestrator run is returned
    /// instead. Once the video outbound transformer pipeline is wired, this
    /// method should round-trip through the provider like Go does.
    async fn get_task_by_external_id(
        &self,
        project_id: i64,
        external_id: &str,
    ) -> Result<OpenAiHandlerResponse, ConduitError> {
        // P-23: the repository lookup includes project_id. A foreign task id
        // therefore resolves to "not found" on every database backend.
        let row = self
            .request_repo
            .find_request_by_external_id_unchecked(
                &Self::ctx(),
                &project_id.to_string(),
                external_id,
            )
            .await
            .map_err(|e| {
                ConduitError::internal(format!("failed to query request by external_id: {e}"))
            })?;

        let Some(row) = row else {
            return Err(ConduitError::not_found(format!(
                "video task not found: {external_id}"
            )));
        };

        // Go returns the provider response body as the wire payload
        // (openai.go:440-444 via VideoInboundTransformer.TransformResponse).
        // Without the transformer we return the stored snapshot verbatim.
        let body_bytes = match row.response_body {
            Some(body) if !body.is_null() => serde_json::to_vec(&body).map_err(|error| {
                ConduitError::internal(format!("failed to encode video task response: {error}"))
            })?,
            _ => {
                // No response stored yet — the task may still be queued/processing.
                // Return an empty JSON object with 200 OK, mirroring Go's
                // `video.Video` zero-value serialisation on a fresh task.
                b"{}".to_vec()
            }
        };

        Ok(OpenAiHandlerResponse::ok_json(body_bytes))
    }

    /// Mirrors Go `VideoService.DeleteTaskByExternalID` (video.go:77-93).
    ///
    /// Marks the local request row as `canceled` (Go `request.StatusCanceled`).
    ///
    /// DEFER: the provider-side delete HTTP call
    /// (`BuildDeleteVideoTaskRequest` + `ch.HTTPClient.Do`) is not wired.
    /// Go deletes the provider task FIRST and then best-effort cancels the
    /// local row (video.go:95-115). The upstream delete will be wired once
    /// the video outbound transformer pipeline is available.
    async fn delete_task_by_external_id(
        &self,
        project_id: i64,
        external_id: &str,
    ) -> Result<(), ConduitError> {
        // P-23: find the request row by external_id AND project, so a key from
        // another project cannot cancel this task (foreign id -> not found).
        let project_id = project_id.to_string();
        let row = self
            .request_repo
            .find_request_by_external_id_unchecked(&Self::ctx(), &project_id, external_id)
            .await
            .map_err(|e| {
                ConduitError::internal(format!("failed to query request by external_id: {e}"))
            })?;

        let Some(row) = row else {
            return Err(ConduitError::not_found(format!(
                "video task not found: {external_id}"
            )));
        };

        if row.status == STATUS_CANCELED {
            return Ok(());
        }

        // RequestRepo owns backend-specific SQL and enforces the lifecycle
        // transition. Video tasks are cancellable while pending/processing.
        let changed = self
            .request_repo
            .transition_request_status_unchecked(
                &Self::ctx(),
                &project_id,
                &row.id,
                &row.status,
                STATUS_CANCELED,
            )
            .await
            .map_err(|e| {
                ConduitError::internal(format!("failed to cancel video task {}: {e}", row.id))
            })?;
        if changed.is_none() {
            return Err(ConduitError::invalid_request(format!(
                "video task {external_id} cannot be canceled from status {}",
                row.status
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use conduit_db::PgRequestRepo;
    use conduit_http::VideoService;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn postgres_video_tasks_are_project_scoped_and_cancellable() -> TestResult {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (project_id, source, model_id, format, request_body, response_body, \
              external_id, status, stream, client_ip, content_saved) \
             VALUES (41, 'api', 'video-model', 'openai/videos', '{}'::jsonb, \
                     '{\"id\":\"task-pg\",\"status\":\"processing\"}'::jsonb, \
                     'task-pg', 'processing', FALSE, '127.0.0.1', FALSE) \
             RETURNING id",
        )
        .fetch_one(&pool)
        .await?;
        let adapter = VideoAdapter::new(Arc::new(PgRequestRepo::new(pool.clone())));

        assert!(
            adapter
                .get_task_by_external_id(99, "task-pg")
                .await
                .is_err(),
            "a foreign project must not read the video task"
        );
        let response = adapter.get_task_by_external_id(41, "task-pg").await?;
        assert_eq!(response.status, 200);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response.body)?,
            serde_json::json!({"id": "task-pg", "status": "processing"})
        );

        adapter.delete_task_by_external_id(41, "task-pg").await?;
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM requests WHERE id = $1")
            .bind(request_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, STATUS_CANCELED);

        database.cleanup().await?;
        Ok(())
    }
}

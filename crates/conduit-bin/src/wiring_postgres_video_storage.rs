//! PostgreSQL executor for the video-storage background scan.

use std::sync::Arc;

use conduit_db::repo::data_storage_repo::DataStorageRepo;
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::{SystemService, VideoStorageSettings, extract_video_url_from_response_body};
use conduit_storage::{DataStorageConfig, DataStorageKind, DataStorageService};
use reqwest::header::CONTENT_DISPOSITION;
use sqlx::{PgPool, Row, postgres::PgRow};

const MAX_VIDEO_BYTES: u64 = 512 * 1024 * 1024;

pub struct PgVideoStorageAdapter {
    pool: PgPool,
    system: Arc<SystemService>,
    data_storage_repo: Arc<dyn DataStorageRepo>,
    client: reqwest::Client,
    last_scan_at: std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>,
}

impl PgVideoStorageAdapter {
    pub fn new(
        pool: PgPool,
        system: Arc<SystemService>,
        data_storage_repo: Arc<dyn DataStorageRepo>,
    ) -> Self {
        Self {
            pool,
            system,
            data_storage_repo,
            client: reqwest::Client::new(),
            last_scan_at: std::sync::Mutex::new(None),
        }
    }

    fn claim_scan(&self, now: chrono::DateTime<chrono::Utc>, interval_minutes: i64) -> bool {
        let Ok(mut last_scan_at) = self.last_scan_at.lock() else {
            return false;
        };
        if !video_scan_due(*last_scan_at, now, interval_minutes) {
            return false;
        }
        *last_scan_at = Some(now);
        true
    }

    async fn run(&self) -> Result<(), String> {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let settings = self
            .system
            .get_json::<VideoStorageSettings>(
                &ctx,
                conduit_services::system_service::system_key::VIDEO_STORAGE_SETTINGS,
            )
            .await
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        if !settings.enabled {
            return Ok(());
        }
        if !self.claim_scan(
            chrono::Utc::now(),
            settings.effective_scan_interval_minutes(),
        ) {
            return Ok(());
        }
        if settings.data_storage_id == 0 {
            return Err("video storage enabled but data_storage_id is not set".to_string());
        }
        let storage = self
            .data_storage_repo
            .find_data_storage_unchecked(&ctx, &settings.data_storage_id.to_string())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("video data storage {} not found", settings.data_storage_id))?;
        if storage.primary || storage.storage_type == "database" {
            return Err("video storage must be non-database storage".to_string());
        }
        if storage.status != "active" {
            return Err(format!("video data storage {} is not active", storage.id));
        }
        let config = DataStorageConfig::from_value(&storage.settings)
            .map_err(|error| format!("invalid video storage settings: {error}"))?;
        let service = DataStorageService::new(storage_kind(&storage.storage_type), Some(&config))
            .map_err(|error| error.to_string())?;
        let rows = sqlx::query(
            "SELECT id, project_id, response_body FROM requests \
             WHERE status IN ('processing', 'completed') \
             AND format IN ('openai/video', 'seedance/video') \
             AND content_saved = FALSE ORDER BY id LIMIT $1",
        )
        .bind(i64::from(settings.effective_scan_limit()))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| error.to_string())?;

        for row in rows {
            let request_id: i64 = row.get("id");
            if let Err(error) = self
                .process_one(&service, settings.data_storage_id, &row)
                .await
            {
                tracing::warn!(request_id, %error, "failed to save PostgreSQL video request");
            }
        }
        Ok(())
    }

    async fn process_one(
        &self,
        service: &DataStorageService,
        storage_id: i64,
        row: &PgRow,
    ) -> Result<(), String> {
        let raw = row
            .try_get::<Option<sqlx::types::Json<serde_json::Value>>, _>("response_body")
            .unwrap_or_default()
            .and_then(|value| serde_json::to_vec(&value.0).ok())
            .unwrap_or_default();
        let Some(url) = extract_video_url_from_response_body(&raw) else {
            return Ok(());
        };
        let mut response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5 * 60))
            .send()
            .await
            .map_err(|error| format!("failed to download video: {error}"))?
            .error_for_status()
            .map_err(|error| format!("failed to download video: {error}"))?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_VIDEO_BYTES)
        {
            return Err(format!("video exceeds {MAX_VIDEO_BYTES} byte limit"));
        }
        let filename = response_filename(&response, &url);
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("failed to read video: {error}"))?
        {
            let next_size = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if next_size > MAX_VIDEO_BYTES {
                return Err(format!("video exceeds {MAX_VIDEO_BYTES} byte limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let request_id: i64 = row.get("id");
        let project_id: i64 = row.get("project_id");
        let key = format!("{project_id}/requests/{request_id}/video/{filename}");
        service
            .save_data(&key, &bytes)
            .await
            .map_err(|error| format!("failed to save video to storage: {error}"))?;
        sqlx::query(
            "UPDATE requests SET content_saved = TRUE, content_storage_id = $1, \
             content_storage_key = $2, content_saved_at = now(), updated_at = now() \
             WHERE id = $3 AND content_saved = FALSE",
        )
        .bind(storage_id)
        .bind(&key)
        .bind(request_id)
        .execute(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn video_scan_due(
    last_scan_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    interval_minutes: i64,
) -> bool {
    let Some(last_scan_at) = last_scan_at else {
        return true;
    };
    now.signed_duration_since(last_scan_at) >= chrono::Duration::minutes(interval_minutes.max(1))
}

impl conduit_scheduler::VideoStorageExecutor for PgVideoStorageAdapter {
    fn scan_and_save(&self) -> Result<(), String> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(self.run()))
    }
}

fn storage_kind(raw: &str) -> DataStorageKind {
    match raw {
        "fs" => DataStorageKind::Local,
        "s3" => DataStorageKind::S3,
        "gcs" => DataStorageKind::Gcs,
        "webdav" => DataStorageKind::WebDav,
        other => DataStorageKind::Unknown(other.to_string()),
    }
}

fn response_filename(response: &reqwest::Response, fallback_url: &str) -> String {
    if let Some(value) = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        && let Some((_, filename)) = value.split_once("filename=")
    {
        let filename = filename.trim().trim_matches('"');
        if !filename.is_empty() {
            return sanitize_filename(filename);
        }
    }
    let path = fallback_url.split('?').next().unwrap_or(fallback_url);
    sanitize_filename(
        path.rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("video.mp4"),
    )
}

fn sanitize_filename(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.is_empty() && *value != "." && *value != "..")
        .unwrap_or("video.mp4")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::NoopCache;
    use serde_json::json;
    use sqlx::types::Json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn filename_sanitization_strips_path_traversal() {
        assert_eq!(sanitize_filename("../clip.mp4"), "clip.mp4");
        assert_eq!(sanitize_filename(".."), "video.mp4");
    }

    #[test]
    fn scan_due_uses_current_interval_instead_of_startup_interval() {
        let now = chrono::Utc::now();
        let two_minutes_ago = now - chrono::Duration::minutes(2);

        assert!(!video_scan_due(Some(two_minutes_ago), now, 60));
        assert!(video_scan_due(Some(two_minutes_ago), now, 1));
        assert!(video_scan_due(None, now, 60));
    }

    #[tokio::test]
    async fn postgres_scan_downloads_video_and_marks_request_saved_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"video-bytes"))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir()?;
        let storage_settings = json!({"directory": directory.path().to_string_lossy()});
        let storage_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO data_storages \
             (name, description, \"primary\", \"type\", settings, status) \
             VALUES ('videos', '', FALSE, 'fs', $1, 'active') RETURNING id",
        )
        .bind(Json(storage_settings.clone()))
        .fetch_one(&database.pool)
        .await?;
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (project_id, model_id, format, request_body, response_body, status) \
             VALUES (3, 'video-model', 'openai/video', '{}'::jsonb, $1, 'completed') \
             RETURNING id",
        )
        .bind(Json(
            json!({"video_url": format!("{}/clip.mp4", server.uri())}),
        ))
        .fetch_one(&database.pool)
        .await?;
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(database.pool.clone())),
            Arc::new(NoopCache::new()),
        ));
        system
            .set_json(
                &RequestContext::new(PolicyContext::new(Principal::system())),
                conduit_services::system_service::system_key::VIDEO_STORAGE_SETTINGS,
                &VideoStorageSettings {
                    enabled: true,
                    data_storage_id: storage_id,
                    scan_interval_minutes: 1,
                    scan_limit: 50,
                },
            )
            .await?;
        PgVideoStorageAdapter::new(
            database.pool.clone(),
            system,
            Arc::new(conduit_db::PgDataStorageRepo::new(database.pool.clone())),
        )
        .run()
        .await?;
        let row = sqlx::query(
            "SELECT content_saved, content_storage_id, content_storage_key \
             FROM requests WHERE id = $1",
        )
        .bind(request_id)
        .fetch_one(&database.pool)
        .await?;
        assert!(row.get::<bool, _>("content_saved"));
        assert_eq!(row.get::<i64, _>("content_storage_id"), storage_id);
        let key: String = row.get("content_storage_key");
        assert_eq!(key, format!("3/requests/{request_id}/video/clip.mp4"));
        let storage_config = DataStorageConfig::from_value(&storage_settings)?;
        let storage_service =
            DataStorageService::new(DataStorageKind::Local, Some(&storage_config))?;
        assert_eq!(storage_service.load_data(&key).await?, b"video-bytes");
        database.cleanup().await?;
        Ok(())
    }
}

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub type FileServiceResult<T> = Result<T, FileServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileServiceError {
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("file repository lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePurpose {
    Assistants,
    Batch,
    FineTune,
    UserData,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileVisibility {
    Private,
    Project,
    Public,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub project_id: String,
    pub storage_key: String,
    pub filename: Option<String>,
    pub content_type: String,
    pub size_bytes: u64,
    pub checksum: Option<String>,
    pub purpose: FilePurpose,
    pub visibility: FileVisibility,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl FileRecord {
    pub fn new(
        project_id: impl Into<String>,
        storage_key: impl Into<String>,
        content_type: impl Into<String>,
        size_bytes: u64,
    ) -> Self {
        let project_id = project_id.into();
        let storage_key = storage_key.into();
        Self {
            id: scoped_file_id(&project_id, &storage_key),
            project_id,
            storage_key,
            filename: None,
            content_type: content_type.into(),
            size_bytes,
            checksum: None,
            purpose: FilePurpose::Other,
            visibility: FileVisibility::Private,
            created_at: Utc::now(),
            deleted_at: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    pub fn with_checksum(mut self, checksum: impl Into<String>) -> Self {
        self.checksum = Some(checksum.into());
        self
    }

    pub fn with_purpose(mut self, purpose: FilePurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn with_visibility(mut self, visibility: FileVisibility) -> Self {
        self.visibility = visibility;
        self
    }
}

#[async_trait]
pub trait FileRepo: Send + Sync {
    async fn create_file(
        &self,
        ctx: &RequestContext,
        record: FileRecord,
    ) -> FileServiceResult<FileRecord>;

    async fn list_files(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> FileServiceResult<Vec<FileRecord>>;

    async fn find_file(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<Option<FileRecord>>;

    async fn delete_file(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<Option<FileRecord>>;
}

pub struct FileService {
    repo: Arc<dyn FileRepo>,
}

impl FileService {
    pub fn new(repo: Arc<dyn FileRepo>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        ctx: &RequestContext,
        record: FileRecord,
    ) -> FileServiceResult<FileRecord> {
        self.repo.create_file(ctx, record).await
    }

    pub async fn list(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> FileServiceResult<Vec<FileRecord>> {
        self.repo.list_files(ctx, project_id).await
    }

    pub async fn get(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<FileRecord> {
        self.repo
            .find_file(ctx, project_id, file_id)
            .await?
            .ok_or_else(|| FileServiceError::FileNotFound(file_id.to_string()))
    }

    pub async fn delete(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<FileRecord> {
        self.repo
            .delete_file(ctx, project_id, file_id)
            .await?
            .ok_or_else(|| FileServiceError::FileNotFound(file_id.to_string()))
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryFileRepo {
    inner: Arc<Mutex<BTreeMap<(String, String), FileRecord>>>,
}

impl InMemoryFileRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file_count(&self) -> FileServiceResult<usize> {
        Ok(self.lock()?.len())
    }

    fn lock(
        &self,
    ) -> FileServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), FileRecord>>> {
        self.inner
            .lock()
            .map_err(|_| FileServiceError::LockPoisoned)
    }
}

#[async_trait]
impl FileRepo for InMemoryFileRepo {
    async fn create_file(
        &self,
        _ctx: &RequestContext,
        record: FileRecord,
    ) -> FileServiceResult<FileRecord> {
        let mut inner = self.lock()?;
        inner.insert(
            (record.project_id.clone(), record.id.clone()),
            record.clone(),
        );
        Ok(record)
    }

    async fn list_files(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> FileServiceResult<Vec<FileRecord>> {
        Ok(self
            .lock()?
            .values()
            .filter(|record| record.project_id == project_id && record.deleted_at.is_none())
            .cloned()
            .collect())
    }

    async fn find_file(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<Option<FileRecord>> {
        Ok(self
            .lock()?
            .get(&(project_id.to_string(), file_id.to_string()))
            .filter(|record| record.deleted_at.is_none())
            .cloned())
    }

    async fn delete_file(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        file_id: &str,
    ) -> FileServiceResult<Option<FileRecord>> {
        let mut inner = self.lock()?;
        let record = inner.get_mut(&(project_id.to_string(), file_id.to_string()));
        Ok(record.map(|record| {
            record.deleted_at.get_or_insert_with(Utc::now);
            record.clone()
        }))
    }
}

fn scoped_file_id(project_id: &str, storage_key: &str) -> String {
    format!("file:{project_id}:{storage_key}")
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn file(project_id: &str, storage_key: &str) -> FileRecord {
        FileRecord::new(project_id, storage_key, "application/json", 42)
            .with_filename("payload.json")
            .with_checksum("sha256:abc123")
            .with_purpose(FilePurpose::Batch)
            .with_visibility(FileVisibility::Project)
    }

    #[tokio::test]
    async fn create_persists_file_metadata() -> FileServiceResult<()> {
        let repo = Arc::new(InMemoryFileRepo::new());
        let service = FileService::new(repo.clone());
        let ctx = ctx();

        let created = service.create(&ctx, file("project-a", "files/a")).await?;

        assert_eq!(created.project_id, "project-a");
        assert_eq!(created.storage_key, "files/a");
        assert_eq!(created.filename.as_deref(), Some("payload.json"));
        assert_eq!(created.content_type, "application/json");
        assert_eq!(created.size_bytes, 42);
        assert_eq!(created.checksum.as_deref(), Some("sha256:abc123"));
        assert_eq!(created.purpose, FilePurpose::Batch);
        assert_eq!(created.visibility, FileVisibility::Project);
        assert_eq!(repo.file_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn list_and_get_are_project_scoped() -> FileServiceResult<()> {
        let service = FileService::new(Arc::new(InMemoryFileRepo::new()));
        let ctx = ctx();

        let project_a = service.create(&ctx, file("project-a", "files/a")).await?;
        let project_b = service.create(&ctx, file("project-b", "files/a")).await?;

        let listed = service.list(&ctx, "project-a").await?;
        assert_eq!(listed, vec![project_a.clone()]);
        assert_eq!(
            service.get(&ctx, "project-a", &project_a.id).await?,
            project_a
        );
        assert!(matches!(
            service.get(&ctx, "project-a", &project_b.id).await,
            Err(FileServiceError::FileNotFound(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn delete_hides_file_from_get_and_list() -> FileServiceResult<()> {
        let repo = Arc::new(InMemoryFileRepo::new());
        let service = FileService::new(repo.clone());
        let ctx = ctx();
        let created = service.create(&ctx, file("project-a", "files/a")).await?;

        let deleted = service.delete(&ctx, "project-a", &created.id).await?;

        assert!(deleted.deleted_at.is_some());
        assert!(service.list(&ctx, "project-a").await?.is_empty());
        assert!(matches!(
            service.get(&ctx, "project-a", &created.id).await,
            Err(FileServiceError::FileNotFound(_))
        ));
        assert_eq!(repo.file_count()?, 1);
        Ok(())
    }
}

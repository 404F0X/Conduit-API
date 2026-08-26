//! P-04: admin GraphQL `backup` mutation seam.
//!
//! Go exposes `mutation backup(input: BackupOptionsInput!): BackupPayload!`
//! (`backup.resolvers.go:21-33`) — it runs `BackupService.Backup`, base64-
//! encodes the archive bytes into `data`, and returns `{ success, data,
//! message }`. The frontend (`features/system/data/system.ts` `BACKUP_MUTATION`)
//! downloads `data` as a file.
//!
//! The Rust admin schema had **no** backup resolver, so the frontend's
//! "create backup" button hit an unknown-field error. This slice adds the
//! input/payload types + the injected [`BackupExtServices`] seam; the host
//! (`conduit-bin`) wires a concrete impl backed by the ported
//! `conduit_services::BackupService` + its DB `BackupDataSource`.

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};

/// `input BackupOptionsInput` — the six include flags the frontend sends
/// (`system.ts` `BackupOptionsInput`). All required (no `omitempty`); the
/// frontend always sends every flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(rename_fields = "camelCase")]
pub struct BackupOptionsInput {
    pub include_channels: bool,
    pub include_model_prices: bool,
    pub include_models: bool,
    #[graphql(name = "includeAPIKeys")]
    pub include_api_keys: bool,
    pub include_usage_stats: bool,
    pub include_request_logs: bool,
}

/// `type BackupPayload` — Go `BackupPayload` (`backup.graphql:31-35`):
/// `success: Boolean!`, `data: String` (base64 archive), `message: String`.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(rename_fields = "camelCase")]
pub struct BackupPayload {
    pub success: bool,
    #[graphql(name = "data")]
    pub data: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "BackupConflictStrategy")]
pub enum BackupConflictStrategy {
    #[graphql(name = "skip")]
    Skip,
    #[graphql(name = "overwrite")]
    Overwrite,
    #[graphql(name = "error")]
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "RestoreOptionsInput", rename_fields = "camelCase")]
pub struct RestoreOptionsInput {
    pub include_channels: bool,
    pub include_model_prices: bool,
    pub include_models: bool,
    #[graphql(name = "includeAPIKeys")]
    pub include_api_keys: bool,
    pub include_usage_stats: bool,
    pub include_request_logs: bool,
    pub channel_conflict_strategy: BackupConflictStrategy,
    pub model_conflict_strategy: BackupConflictStrategy,
    pub model_price_conflict_strategy: BackupConflictStrategy,
    #[graphql(name = "apiKeyConflictStrategy")]
    pub api_key_conflict_strategy: BackupConflictStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "RestorePayload")]
pub struct RestorePayload {
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "TriggerBackupPayload")]
pub struct TriggerBackupPayload {
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupExtError {
    #[error("backup service is not available")]
    ServiceUnavailable,
    #[error("backup failed: {0}")]
    Backup(String),
    #[error("restore failed: {0}")]
    Restore(String),
}

/// The injected backup service seam. The host wires a concrete type backed by
/// `conduit_services::BackupService::dump` + the DB `BackupDataSource`.
///
/// `run_backup` returns the **base64-encoded** archive bytes (Go
/// `base64.StdEncoding.EncodeToString(data)`, `backup.resolvers.go:27`), so the
/// resolver stays free of encoding concerns.
#[async_trait::async_trait]
pub trait BackupExtServices: Send + Sync {
    async fn run_backup(&self, opts: BackupOptionsInput) -> Result<String, BackupExtError>;
    async fn restore(&self, data: Vec<u8>, opts: RestoreOptionsInput)
    -> Result<(), BackupExtError>;
    async fn trigger_auto_backup(&self) -> Result<(), BackupExtError>;
}

/// Resolve the injected [`BackupExtServices`] from the data bag.
pub(crate) fn backup_ext_services(ctx: &Context<'_>) -> Result<Arc<dyn BackupExtServices>, String> {
    match ctx.data::<Arc<dyn BackupExtServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(BackupExtError::ServiceUnavailable.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    /// Fake backup service returning a fixed base64 payload, recording the
    /// options it was handed so the resolver→service plumbing is asserted.
    struct FakeBackupService {
        seen: std::sync::Mutex<Option<BackupOptionsInput>>,
        data: String,
    }

    #[async_trait::async_trait]
    impl BackupExtServices for FakeBackupService {
        async fn run_backup(&self, opts: BackupOptionsInput) -> Result<String, BackupExtError> {
            if let Ok(mut guard) = self.seen.lock() {
                *guard = Some(opts);
            }
            Ok(self.data.clone())
        }

        async fn restore(
            &self,
            _data: Vec<u8>,
            _opts: RestoreOptionsInput,
        ) -> Result<(), BackupExtError> {
            Ok(())
        }

        async fn trigger_auto_backup(&self) -> Result<(), BackupExtError> {
            Ok(())
        }
    }

    fn schema_with(service: Arc<FakeBackupService>) -> AdminSchema {
        let services: Arc<dyn BackupExtServices> = service;
        admin_schema_builder().data(services).finish()
    }

    /// The `backup` mutation forwards the include flags to the service and wraps
    /// the returned base64 string in `{ success: true, data, message }`
    /// (Go `backup.resolvers.go:21-33`).
    #[tokio::test]
    async fn backup_mutation_forwards_options_and_wraps_payload() -> Result<(), TestError> {
        let service = Arc::new(FakeBackupService {
            seen: std::sync::Mutex::new(None),
            data: "YmFja3VwLWJ5dGVz".to_string(),
        });
        let schema = schema_with(Arc::clone(&service));

        let query = r#"mutation {
            backup(input: {
                includeChannels: true, includeModelPrices: false, includeModels: true,
                includeAPIKeys: false, includeUsageStats: true, includeRequestLogs: false
            }) { success data message }
        }"#;
        let resp = schema.execute(query).await;
        assert!(
            resp.errors.is_empty(),
            "unexpected errors: {:?}",
            resp.errors
        );

        let data = resp.data.into_json()?;
        assert_eq!(data["backup"]["success"], true);
        assert_eq!(data["backup"]["data"], "YmFja3VwLWJ5dGVz");

        // The service saw the exact include flags the mutation carried.
        let seen = service
            .seen
            .lock()
            .map(|g| *g)
            .map_err(|_| "seen lock poisoned")?;
        let seen = seen.ok_or("service was not invoked")?;
        assert!(seen.include_channels);
        assert!(!seen.include_model_prices);
        assert!(seen.include_models);
        assert!(!seen.include_api_keys);
        assert!(seen.include_usage_stats);
        assert!(!seen.include_request_logs);
        Ok(())
    }

    /// With no service wired the resolver surfaces the shared "unavailable"
    /// failure rather than panicking.
    #[tokio::test]
    async fn backup_mutation_without_service_errors() -> Result<(), TestError> {
        let schema: AdminSchema = admin_schema_builder().finish();
        let query = r#"mutation {
            backup(input: {
                includeChannels: true, includeModelPrices: true, includeModels: true,
                includeAPIKeys: true, includeUsageStats: true, includeRequestLogs: true
            }) { success }
        }"#;
        let resp = schema.execute(query).await;
        assert!(
            !resp.errors.is_empty(),
            "expected an error with no service wired"
        );
        Ok(())
    }
}

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};
use async_trait::async_trait;

use crate::scalars::TimeScalar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
#[graphql(name = "DiagnosticsTarget")]
pub enum DiagnosticsTarget {
    ChannelCache,
}

#[derive(Debug, Clone, Default, InputObject)]
#[graphql(name = "GetCacheDiagnosticsInput")]
pub struct GetCacheDiagnosticsInput {
    pub targets: Option<Vec<DiagnosticsTarget>>,
}

#[derive(Debug, Clone, Default, InputObject)]
#[graphql(name = "ClearCacheInput")]
pub struct ClearCacheInput {
    pub targets: Option<Vec<DiagnosticsTarget>>,
}

#[derive(Debug, Clone, SimpleObject)]
#[graphql(name = "GetCacheDiagnosticsPayload")]
pub struct GetCacheDiagnosticsPayload {
    pub file_name: String,
    pub content: String,
    pub targets: Vec<DiagnosticsTarget>,
}

#[derive(Debug, Clone, SimpleObject)]
#[graphql(name = "ClearCachePayload")]
pub struct ClearCachePayload {
    pub success: bool,
    pub message: String,
    pub targets: Vec<DiagnosticsTarget>,
}

#[derive(Debug, Clone, Copy, InputObject)]
#[graphql(name = "TriggerGcCleanupInput")]
pub struct TriggerGcCleanupInput {
    pub requests_cleanup_days: i32,
    pub usage_logs_cleanup_days: i32,
}

#[derive(Debug, Clone, SimpleObject)]
#[graphql(name = "GcCleanupPreviewItem")]
pub struct GcCleanupPreviewItem {
    pub resource_type: String,
    pub estimated_count: i32,
    pub cutoff_time: TimeScalar,
    pub retention_days: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum SystemOperationsError {
    #[error("system operations service is not available")]
    Unavailable,
    #[error("system operation failed: {0}")]
    Operation(String),
}

#[async_trait]
pub trait SystemOperationsServices: Send + Sync {
    async fn get_cache_diagnostics(
        &self,
        input: Option<GetCacheDiagnosticsInput>,
    ) -> Result<GetCacheDiagnosticsPayload, SystemOperationsError>;
    async fn clear_cache(
        &self,
        input: ClearCacheInput,
    ) -> Result<ClearCachePayload, SystemOperationsError>;
    async fn preview_gc_cleanup(
        &self,
        input: TriggerGcCleanupInput,
    ) -> Result<Vec<GcCleanupPreviewItem>, SystemOperationsError>;
    async fn trigger_gc_cleanup(
        &self,
        input: TriggerGcCleanupInput,
    ) -> Result<bool, SystemOperationsError>;
}

pub fn system_operations_services<'a>(
    ctx: &'a Context<'_>,
) -> Result<&'a Arc<dyn SystemOperationsServices>, SystemOperationsError> {
    ctx.data_opt::<Arc<dyn SystemOperationsServices>>()
        .ok_or(SystemOperationsError::Unavailable)
}

pub fn normalize_targets(targets: Option<Vec<DiagnosticsTarget>>) -> Vec<DiagnosticsTarget> {
    let mut targets = targets.unwrap_or_default();
    if targets.is_empty() {
        return vec![DiagnosticsTarget::ChannelCache];
    }
    targets.sort();
    targets.dedup();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_targets_default_and_deduplicate() {
        assert_eq!(
            normalize_targets(None),
            vec![DiagnosticsTarget::ChannelCache]
        );
        assert_eq!(
            normalize_targets(Some(vec![
                DiagnosticsTarget::ChannelCache,
                DiagnosticsTarget::ChannelCache,
            ])),
            vec![DiagnosticsTarget::ChannelCache]
        );
    }
}

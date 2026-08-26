//! Backend-neutral repository contract for channel override templates.
//!
//! Keeping this contract backend-neutral lets the admin adapter use the same
//! behavior across repository implementations. JSON payloads are
//! passed as serialized strings at this boundary because the GraphQL adapter
//! already validates and normalizes the operation arrays.

use async_trait::async_trait;

use crate::repo::RepoResult;
use crate::row::ChannelOverrideTemplateRow;

#[derive(Debug, Clone)]
pub struct CreateChannelOverrideTemplateInput {
    pub user_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub override_parameters: String,
    pub override_headers: String,
    pub header_override_operations: String,
    pub body_override_operations: String,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateChannelOverrideTemplateInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub clear_description: bool,
    pub override_parameters: Option<String>,
    pub override_headers: Option<String>,
    pub header_override_operations: Option<String>,
    pub body_override_operations: Option<String>,
}

#[async_trait]
pub trait ChannelOverrideTemplateRepo: Send + Sync {
    async fn list(&self, user_id: i64) -> RepoResult<Vec<ChannelOverrideTemplateRow>>;

    async fn find(&self, id: i64, user_id: i64) -> RepoResult<Option<ChannelOverrideTemplateRow>>;

    async fn create(
        &self,
        input: CreateChannelOverrideTemplateInput,
    ) -> RepoResult<ChannelOverrideTemplateRow>;

    async fn update(
        &self,
        id: i64,
        user_id: i64,
        input: UpdateChannelOverrideTemplateInput,
    ) -> RepoResult<ChannelOverrideTemplateRow>;

    async fn soft_delete(&self, id: i64, user_id: i64) -> RepoResult<()>;

    async fn channel_settings(&self, channel_id: i64) -> RepoResult<Option<String>>;

    async fn set_channel_settings_batch(&self, updates: &[(i64, String)]) -> RepoResult<()>;
}

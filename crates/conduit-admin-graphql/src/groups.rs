//! Independent user classification and key model scope. Project finance stays separate.
use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};
use std::sync::Arc;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum GroupStatus {
    Enabled,
    Disabled,
    Archived,
}
impl GroupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Archived => "archived",
        }
    }
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ModelGroup {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub status: GroupStatus,
    pub revision: i64,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<ID>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct UserGroup {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub status: GroupStatus,
    #[graphql(name = "rechargeMultiplierPPM")]
    pub recharge_multiplier_ppm: String,
    pub recharge_revision: String,
    #[graphql(name = "memberUserIDs")]
    pub member_user_ids: Vec<ID>,
    #[graphql(name = "modelGroupIDs")]
    pub model_group_ids: Vec<ID>,
}
#[derive(Debug, Clone, InputObject)]
pub struct SaveModelGroupInput {
    pub id: Option<ID>,
    pub name: String,
    pub description: Option<String>,
    pub status: GroupStatus,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<ID>,
}
#[derive(Debug, Clone, InputObject)]
pub struct SaveUserGroupInput {
    pub id: Option<ID>,
    pub name: String,
    pub description: Option<String>,
    pub status: GroupStatus,
    #[graphql(name = "rechargeMultiplierPPM")]
    pub recharge_multiplier_ppm: Option<String>,
    pub expected_recharge_revision: Option<String>,
    #[graphql(name = "memberUserIDs")]
    pub member_user_ids: Vec<ID>,
    #[graphql(name = "modelGroupIDs")]
    pub model_group_ids: Vec<ID>,
}
#[async_trait::async_trait]
pub trait GroupServices: Send + Sync {
    async fn creation_catalog(
        &self,
        user_id: Option<i64>,
        parent_key_id: Option<i64>,
        project_id: i64,
        model_group_id: &str,
    ) -> Result<APIKeyProfileCatalog, String>;
    async fn profile_catalog(
        &self,
        key_id: i64,
        project_id: i64,
    ) -> Result<APIKeyProfileCatalog, String>;
    async fn model_groups(&self) -> Result<Vec<ModelGroup>, String>;
    async fn user_groups(&self) -> Result<Vec<UserGroup>, String>;
    async fn save_model_group(&self, input: SaveModelGroupInput) -> Result<ModelGroup, String>;
    async fn save_user_group(&self, input: SaveUserGroupInput) -> Result<UserGroup, String>;
    async fn move_user(&self, user_id: i64, group_id: &str) -> Result<bool, String>;
    async fn assignable(&self, user_id: i64, project_id: i64) -> Result<Vec<ModelGroup>, String>;
    async fn assignable_for_key(
        &self,
        key_id: i64,
        project_id: i64,
    ) -> Result<Vec<ModelGroup>, String>;
    async fn key_group(&self, key_id: i64, project_id: i64) -> Result<Option<ID>, String>;
    async fn assign_key(
        &self,
        key_id: i64,
        project_id: i64,
        group_id: &str,
    ) -> Result<bool, String>;
}
pub fn services(ctx: &Context<'_>) -> Result<Arc<dyn GroupServices>, String> {
    ctx.data::<Arc<dyn GroupServices>>()
        .cloned()
        .map_err(|_| "group services unavailable".into())
}
pub fn global(ctx: &Context<'_>, write: bool) -> Result<(), String> {
    let scope = if write {
        conduit_auth::scopes::slug::WRITE_USERS
    } else {
        conduit_auth::scopes::slug::READ_USERS
    };
    match crate::policy::AdminAccessScope::from_graphql_context(ctx, scope)
        .map_err(|e| e.to_string())?
    {
        crate::policy::AdminAccessScope::Global => Ok(()),
        _ => Err("global user administration required".into()),
    }
}
pub fn project(ctx: &Context<'_>, write: bool) -> Result<i64, String> {
    crate::policy::authorize_current(
        ctx,
        if write {
            conduit_auth::scopes::slug::WRITE_API_KEYS
        } else {
            conduit_auth::scopes::slug::READ_API_KEYS
        },
    )
    .map_err(|e| e.to_string())?;
    numeric_id(
        crate::policy::request_context(ctx)
            .and_then(|r| r.project_id.as_deref())
            .ok_or("selected Project required")?,
    )
}

pub fn numeric_id(raw: &str) -> Result<i64, String> {
    raw.parse()
        .or_else(|_| crate::node::parse_guid(raw).map(|id| id.id))
        .map_err(|_| "invalid numeric identity".into())
}

/// Management metadata for the target Key, never a quote or a selectable request identity.
#[derive(Debug, Clone, SimpleObject)]
pub struct APIKeyProfileCatalog {
    #[graphql(name = "keyID")]
    pub key_id: Option<ID>,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    #[graphql(name = "billingUserID")]
    pub billing_user_id: Option<ID>,
    #[graphql(name = "modelGroupID")]
    pub model_group_id: Option<ID>,
    pub configured: bool,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<String>,
    pub channels: Vec<APIKeyProfileChannel>,
    pub models: Vec<APIKeyProfileModel>,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct APIKeyProfileChannel {
    pub id: ID,
    pub name: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct APIKeyProfileModel {
    pub id: String,
    pub name: String,
    #[graphql(name = "channelIDs")]
    pub channel_ids: Vec<ID>,
}

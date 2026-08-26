//! Commercialization-v2 admin API (Conduit API Rust extension; not Go parity).

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, Json, SimpleObject};
use serde_json::Value;

use crate::channel::ModelMapping;
use crate::model::{CreateModelInput, Model};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum CommercialStatus {
    Enabled,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct UpstreamModelDeployment {
    pub id: ID,
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub channel_name: String,
    #[graphql(name = "upstreamModelID")]
    pub upstream_model_id: String,
    pub internal_name: String,
    pub variant: String,
    pub status: CommercialStatus,
    pub source: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ModelRoute {
    pub id: ID,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    #[graphql(name = "publicModelKey")]
    pub public_model_key: String,
    #[graphql(name = "deploymentID")]
    pub deployment_id: ID,
    pub deployment_name: String,
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub channel_name: String,
    #[graphql(name = "upstreamModelID")]
    pub upstream_model_id: String,
    pub status: CommercialStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelModelMappingAction {
    Create,
    Skip,
    Conflict,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelModelMappingPreviewEntry {
    pub action: ChannelModelMappingAction,
    pub from: String,
    pub to: String,
    pub previous_to: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelModelMappingPreview {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub expected_version: String,
    pub entries: Vec<ChannelModelMappingPreviewEntry>,
    pub create_count: i32,
    pub skip_count: i32,
    pub conflict_count: i32,
}

#[derive(Debug, Clone, InputObject)]
pub struct ApplyChannelModelMappingsInput {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub expected_version: String,
    /// Conflicting aliases are never overwritten unless this is explicitly true.
    pub replace_conflicts: Option<bool>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct ChannelModelMappingAutomationSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, InputObject)]
pub struct SetChannelModelMappingAutomationInput {
    pub enabled: bool,
}

#[derive(Debug, Clone, InputObject)]
pub struct UpsertModelRouteInput {
    pub id: Option<ID>,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    #[graphql(name = "deploymentID")]
    pub deployment_id: ID,
    pub status: Option<CommercialStatus>,
    /// Required when a second deployment is attached to an existing public
    /// model. Equal upstream names do not prove equal capability or quality.
    pub confirm_compatibility: Option<bool>,
}

#[derive(Debug, Clone, InputObject)]
pub struct CreatePublicModelWithRoutesInput {
    pub model: CreateModelInput,
    #[graphql(name = "deploymentIDs")]
    pub deployment_ids: Vec<ID>,
    pub enabled: Option<bool>,
    pub confirm_compatibility: Option<bool>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct CreatePublicModelWithRoutesPayload {
    pub model: Model,
    pub routes: Vec<ModelRoute>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct PriceBookItem {
    pub id: ID,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    #[graphql(name = "publicModelKey")]
    pub public_model_key: String,
    pub price: Json<Value>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct PriceBookVersion {
    pub id: ID,
    pub version: i32,
    pub status: String,
    #[graphql(name = "referenceID")]
    pub reference_id: String,
    pub effective_start_at: Option<String>,
    pub effective_end_at: Option<String>,
    pub items: Vec<PriceBookItem>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct PriceBook {
    pub id: ID,
    pub name: String,
    pub currency: String,
    pub status: CommercialStatus,
    pub is_default: bool,
    pub versions: Vec<PriceBookVersion>,
}

#[derive(Debug, Clone, InputObject)]
pub struct CreatePriceBookInput {
    pub name: String,
    pub currency: Option<String>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PrimaryProjectResolutionStatus {
    Resolved,
    Missing,
    Ambiguous,
}

/// Strict personal-account Project resolution. A result is resolved only when
/// exactly one active owned Project has an active `personal` commercial
/// profile; callers never receive an arbitrary first Project.
#[derive(Debug, Clone, SimpleObject)]
pub struct PrimaryProjectResolution {
    pub status: PrimaryProjectResolutionStatus,
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "candidateProjectIDs")]
    pub candidate_project_ids: Vec<ID>,
}
#[derive(Debug, thiserror::Error)]
pub enum CommercializationError {
    #[error("commercialization service is unavailable")]
    Unavailable,
    #[error("commercialization object not found: {0}")]
    NotFound(String),
    #[error("invalid commercialization input: {0}")]
    Invalid(String),
    #[error("commercialization operation failed: {0}")]
    Storage(String),
}

/// Compare explicit public-model routes with a channel's current aliases.
/// Route identity, rather than model-name similarity, is the source of truth.
pub fn build_channel_model_mapping_preview(
    channel_id: ID,
    expected_version: String,
    existing: &[ModelMapping],
    routed: &[(String, String)],
) -> ChannelModelMappingPreview {
    use std::collections::BTreeMap;

    let current = existing
        .iter()
        .map(|mapping| (mapping.from.as_str(), mapping.to.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = BTreeMap::<&str, &str>::new();
    for (from, to) in routed {
        candidates.insert(from.as_str(), to.as_str());
    }

    let entries = candidates
        .into_iter()
        .map(|(from, to)| match current.get(from).copied() {
            None => ChannelModelMappingPreviewEntry {
                action: ChannelModelMappingAction::Create,
                from: from.to_string(),
                to: to.to_string(),
                previous_to: None,
                reason: "route is not present in channel aliases".into(),
            },
            Some(previous) if previous == to => ChannelModelMappingPreviewEntry {
                action: ChannelModelMappingAction::Skip,
                from: from.to_string(),
                to: to.to_string(),
                previous_to: Some(previous.to_string()),
                reason: "channel alias already matches the route".into(),
            },
            Some(previous) => ChannelModelMappingPreviewEntry {
                action: ChannelModelMappingAction::Conflict,
                from: from.to_string(),
                to: to.to_string(),
                previous_to: Some(previous.to_string()),
                reason: "channel alias points to another upstream model".into(),
            },
        })
        .collect::<Vec<_>>();
    ChannelModelMappingPreview {
        channel_id,
        expected_version,
        create_count: entries
            .iter()
            .filter(|entry| entry.action == ChannelModelMappingAction::Create)
            .count() as i32,
        skip_count: entries
            .iter()
            .filter(|entry| entry.action == ChannelModelMappingAction::Skip)
            .count() as i32,
        conflict_count: entries
            .iter()
            .filter(|entry| entry.action == ChannelModelMappingAction::Conflict)
            .count() as i32,
        entries,
    }
}

/// Merge a reviewed preview without deleting aliases unrelated to its routes.
pub fn merge_channel_model_mappings(
    existing: &[ModelMapping],
    preview: &ChannelModelMappingPreview,
    replace_conflicts: bool,
) -> Vec<ModelMapping> {
    let mut merged = existing.to_vec();
    for entry in &preview.entries {
        match entry.action {
            ChannelModelMappingAction::Skip => {}
            ChannelModelMappingAction::Create => merged.push(ModelMapping {
                from: entry.from.clone(),
                to: entry.to.clone(),
            }),
            ChannelModelMappingAction::Conflict if replace_conflicts => {
                if let Some(mapping) = merged.iter_mut().find(|item| item.from == entry.from) {
                    mapping.to.clone_from(&entry.to);
                }
            }
            ChannelModelMappingAction::Conflict => {}
        }
    }
    merged
}

#[async_trait::async_trait]
pub trait CommercializationServices: Send + Sync {
    async fn primary_project_for_user(
        &self,
        user_id: &str,
    ) -> Result<PrimaryProjectResolution, CommercializationError>;
    async fn upstream_model_deployments(
        &self,
        channel_id: Option<&str>,
    ) -> Result<Vec<UpstreamModelDeployment>, CommercializationError>;
    async fn model_routes(
        &self,
        public_model_id: Option<&str>,
    ) -> Result<Vec<ModelRoute>, CommercializationError>;
    async fn channel_model_mapping_automation_settings(
        &self,
    ) -> Result<ChannelModelMappingAutomationSettings, CommercializationError>;
    async fn set_channel_model_mapping_automation(
        &self,
        input: SetChannelModelMappingAutomationInput,
    ) -> Result<ChannelModelMappingAutomationSettings, CommercializationError>;
    async fn preview_channel_model_mappings(
        &self,
        channel_id: &str,
    ) -> Result<ChannelModelMappingPreview, CommercializationError>;
    async fn apply_channel_model_mappings(
        &self,
        input: ApplyChannelModelMappingsInput,
    ) -> Result<ChannelModelMappingPreview, CommercializationError>;
    async fn upsert_model_route(
        &self,
        input: UpsertModelRouteInput,
    ) -> Result<ModelRoute, CommercializationError>;
    async fn create_public_model_with_routes(
        &self,
        input: CreatePublicModelWithRoutesInput,
    ) -> Result<CreatePublicModelWithRoutesPayload, CommercializationError>;
    async fn price_books(&self) -> Result<Vec<PriceBook>, CommercializationError>;
    async fn create_price_book(
        &self,
        actor_user_id: Option<i64>,
        input: CreatePriceBookInput,
    ) -> Result<PriceBook, CommercializationError>;
}

pub(crate) fn commercialization_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn CommercializationServices>, String> {
    ctx.data::<Arc<dyn CommercializationServices>>()
        .cloned()
        .map_err(|_| CommercializationError::Unavailable.to_string())
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    fn mapping(from: &str, to: &str) -> ModelMapping {
        ModelMapping {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn preview_classifies_create_skip_and_conflict() {
        let preview = build_channel_model_mapping_preview(
            ID("channel-1".into()),
            "v1".into(),
            &[mapping("same", "up-a"), mapping("conflict", "old")],
            &[
                ("same".into(), "up-a".into()),
                ("new".into(), "up-b".into()),
                ("conflict".into(), "up-c".into()),
            ],
        );
        assert_eq!(preview.create_count, 1);
        assert_eq!(preview.skip_count, 1);
        assert_eq!(preview.conflict_count, 1);
    }

    #[test]
    fn merge_preserves_unrelated_and_requires_conflict_confirmation() {
        let existing = vec![mapping("unrelated", "keep"), mapping("sku", "old")];
        let preview = build_channel_model_mapping_preview(
            ID("channel-1".into()),
            "v1".into(),
            &existing,
            &[
                ("sku".into(), "new".into()),
                ("added".into(), "target".into()),
            ],
        );
        let safe = merge_channel_model_mappings(&existing, &preview, false);
        assert_eq!(
            safe,
            vec![
                mapping("unrelated", "keep"),
                mapping("sku", "old"),
                mapping("added", "target")
            ]
        );
        let replaced = merge_channel_model_mappings(&existing, &preview, true);
        assert_eq!(
            replaced,
            vec![
                mapping("unrelated", "keep"),
                mapping("sku", "new"),
                mapping("added", "target")
            ]
        );
    }
}

//! Backend-neutral API-key profile-template repository contract.

use async_trait::async_trait;
use serde_json::Value;

use crate::policy::ProjectAccess;
use crate::repo::{RepoResult, RequestContext, guard_project_access};
pub use crate::row::ApiKeyProfileTemplateRow;

#[derive(Debug, Clone)]
pub struct CreateProfileTemplateInput {
    pub project_id: String,
    pub name: String,
    pub description: Option<String>,
    pub profile: Option<Value>,
    pub created_at: String,
}

#[derive(Debug, Default, Clone)]
pub struct UpdateProfileTemplateInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub profile: Option<Value>,
    pub updated_at: String,
}

#[async_trait]
pub trait ProfileTemplateRepo: Send + Sync {
    async fn create_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow>;

    async fn create_profile_template(
        &self,
        ctx: &RequestContext,
        input: CreateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        self.create_profile_template_unchecked(ctx, input).await
    }

    async fn find_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>>;

    async fn find_profile_template_by_id_unchecked(
        &self,
        ctx: &RequestContext,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>>;

    async fn find_profile_template(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_profile_template_unchecked(ctx, project_id, template_id)
            .await
    }

    async fn find_profile_template_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>>;

    async fn find_profile_template_with_deleted(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_profile_template_with_deleted_unchecked(ctx, project_id, template_id)
            .await
    }

    async fn list_profile_templates_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<ApiKeyProfileTemplateRow>>;

    async fn list_all_profile_templates_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<ApiKeyProfileTemplateRow>>;

    async fn list_profile_templates(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<ApiKeyProfileTemplateRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.list_profile_templates_unchecked(ctx, project_id).await
    }

    async fn update_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        input: UpdateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow>;

    async fn update_profile_template(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        input: UpdateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.update_profile_template_unchecked(ctx, project_id, template_id, input)
            .await
    }

    async fn soft_delete_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        deleted_at: String,
    ) -> RepoResult<ApiKeyProfileTemplateRow>;

    async fn soft_delete_profile_template(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        deleted_at: String,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.soft_delete_profile_template_unchecked(ctx, project_id, template_id, deleted_at)
            .await
    }
}

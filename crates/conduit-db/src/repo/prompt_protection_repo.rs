//! Backend-neutral prompt-protection-rule repository contract.

use async_trait::async_trait;
use serde_json::Value;

use crate::repo::{RepoResult, RequestContext, guard_repo_principal};
use crate::row::PromptProtectionRuleRow;

pub const RULE_STATUS_ENABLED: &str = "enabled";
pub const RULE_STATUS_DISABLED: &str = "disabled";
pub const RULE_STATUS_ARCHIVED: &str = "archived";

#[derive(Debug, Clone)]
pub struct CreateProtectionRuleInput {
    pub name: String,
    pub description: Option<String>,
    pub pattern: String,
    pub settings: Value,
    pub created_at: String,
}

#[derive(Debug, Default, Clone)]
pub struct UpdateProtectionRuleInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub pattern: Option<String>,
    pub status: Option<String>,
    pub settings: Option<Value>,
    pub updated_at: String,
}

#[async_trait]
pub trait PromptProtectionRuleRepo: Send + Sync {
    async fn create_protection_rule_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow>;

    async fn create_protection_rule(
        &self,
        ctx: &RequestContext,
        input: CreateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow> {
        guard_repo_principal(ctx)?;
        self.create_protection_rule_unchecked(ctx, input).await
    }

    async fn find_protection_rule_unchecked(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>>;

    async fn find_protection_rule(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>> {
        guard_repo_principal(ctx)?;
        self.find_protection_rule_unchecked(ctx, rule_id).await
    }

    async fn find_protection_rule_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>>;

    async fn find_protection_rule_with_deleted(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>> {
        guard_repo_principal(ctx)?;
        self.find_protection_rule_with_deleted_unchecked(ctx, rule_id)
            .await
    }

    async fn list_protection_rules_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>>;

    async fn list_protection_rules(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        guard_repo_principal(ctx)?;
        self.list_protection_rules_unchecked(ctx).await
    }

    async fn list_enabled_protection_rules_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>>;

    async fn list_enabled_protection_rules(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        guard_repo_principal(ctx)?;
        self.list_enabled_protection_rules_unchecked(ctx).await
    }

    async fn update_protection_rule_unchecked(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
        input: UpdateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow>;

    async fn update_protection_rule(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
        input: UpdateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow> {
        guard_repo_principal(ctx)?;
        self.update_protection_rule_unchecked(ctx, rule_id, input)
            .await
    }

    async fn set_protection_rule_status_unchecked(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
        status: &str,
        updated_at: String,
    ) -> RepoResult<PromptProtectionRuleRow>;

    async fn set_protection_rule_status(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
        status: &str,
        updated_at: String,
    ) -> RepoResult<PromptProtectionRuleRow> {
        guard_repo_principal(ctx)?;
        self.set_protection_rule_status_unchecked(ctx, rule_id, status, updated_at)
            .await
    }

    async fn soft_delete_protection_rule_unchecked(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<PromptProtectionRuleRow>;

    async fn soft_delete_protection_rule(
        &self,
        ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<PromptProtectionRuleRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_protection_rule_unchecked(ctx, rule_id)
            .await
    }

    async fn bulk_delete_protection_rules_unchecked(
        &self,
        ctx: &RequestContext,
        rule_ids: &[String],
    ) -> RepoResult<u64>;

    async fn bulk_delete_protection_rules(
        &self,
        ctx: &RequestContext,
        rule_ids: &[String],
    ) -> RepoResult<u64> {
        guard_repo_principal(ctx)?;
        self.bulk_delete_protection_rules_unchecked(ctx, rule_ids)
            .await
    }

    async fn bulk_set_protection_rule_status_unchecked(
        &self,
        ctx: &RequestContext,
        rule_ids: &[String],
        status: &str,
    ) -> RepoResult<u64>;

    async fn bulk_set_protection_rule_status(
        &self,
        ctx: &RequestContext,
        rule_ids: &[String],
        status: &str,
    ) -> RepoResult<u64> {
        guard_repo_principal(ctx)?;
        self.bulk_set_protection_rule_status_unchecked(ctx, rule_ids, status)
            .await
    }
}

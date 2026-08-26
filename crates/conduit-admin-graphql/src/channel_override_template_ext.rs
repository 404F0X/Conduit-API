//! P-54: channel override template GraphQL contract and host service seam.

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::{OrderDirection, OverrideOperation, OverrideOperationInput};
use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, TimeScalar};
use crate::system_settings_ext::HeaderEntry;
use crate::user::User;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "OverrideApplyMode")]
pub enum OverrideApplyMode {
    #[graphql(name = "MERGE")]
    Merge,
    #[graphql(name = "REPLACE")]
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "ChannelOverrideTemplateOrderField")]
pub enum ChannelOverrideTemplateOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "ChannelOverrideTemplateOrder")]
pub struct ChannelOverrideTemplateOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: ChannelOverrideTemplateOrderField,
}

/// The frontend currently filters templates by `nameContainsFold`. The scalar
/// equality form is included because it is part of the captured Go schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "ChannelOverrideTemplateWhereInput")]
pub struct ChannelOverrideTemplateWhereInput {
    pub name: Option<String>,
    pub name_contains: Option<String>,
    pub name_contains_fold: Option<String>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ChannelOverrideTemplate")]
pub struct ChannelOverrideTemplate {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    #[graphql(name = "userID")]
    pub user_id: Option<ID>,
    pub name: String,
    pub description: Option<String>,
    #[graphql(deprecation = "Use bodyOverrideOperations instead")]
    pub override_parameters: String,
    #[graphql(deprecation)]
    pub override_headers: Vec<HeaderEntry>,
    pub header_override_operations: Vec<OverrideOperation>,
    pub body_override_operations: Vec<OverrideOperation>,
    pub user: Option<User>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ChannelOverrideTemplateEdge")]
pub struct ChannelOverrideTemplateEdge {
    pub node: Option<ChannelOverrideTemplate>,
    pub cursor: CursorScalar,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ChannelOverrideTemplateConnection")]
pub struct ChannelOverrideTemplateConnection {
    pub edges: Option<Vec<Option<ChannelOverrideTemplateEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

#[derive(Debug, Clone, InputObject)]
#[graphql(name = "CreateChannelOverrideTemplateInput")]
pub struct CreateChannelOverrideTemplateInput {
    pub name: String,
    pub description: Option<String>,
    pub header_override_operations: Option<Vec<OverrideOperationInput>>,
    pub body_override_operations: Option<Vec<OverrideOperationInput>>,
}

#[derive(Debug, Clone, Default, InputObject)]
#[graphql(name = "UpdateChannelOverrideTemplateInput")]
pub struct UpdateChannelOverrideTemplateInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub clear_description: Option<bool>,
    pub header_override_operations: Option<Vec<OverrideOperationInput>>,
    pub append_header_override_operations: Option<Vec<OverrideOperationInput>>,
    pub clear_header_override_operations: Option<bool>,
    pub body_override_operations: Option<Vec<OverrideOperationInput>>,
    pub append_body_override_operations: Option<Vec<OverrideOperationInput>>,
    pub clear_body_override_operations: Option<bool>,
}

#[derive(Debug, Clone, InputObject)]
#[graphql(name = "ApplyChannelOverrideTemplateInput")]
pub struct ApplyChannelOverrideTemplateInput {
    #[graphql(name = "templateID")]
    pub template_id: ID,
    #[graphql(name = "channelIDs")]
    pub channel_ids: Vec<ID>,
    pub mode: Option<OverrideApplyMode>,
}

#[derive(Debug, Clone, InputObject)]
#[graphql(name = "ClearChannelOverrideTemplatesInput")]
pub struct ClearChannelOverrideTemplatesInput {
    #[graphql(name = "channelIDs")]
    pub channel_ids: Vec<ID>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ApplyChannelOverrideTemplatePayload")]
pub struct ApplyChannelOverrideTemplatePayload {
    pub success: bool,
    pub updated: i32,
    pub channels: Vec<crate::channel::Channel>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ClearChannelOverrideTemplatesPayload")]
pub struct ClearChannelOverrideTemplatesPayload {
    pub success: bool,
    pub updated: i32,
    pub channels: Vec<crate::channel::Channel>,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelOverrideTemplateConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ChannelOverrideTemplateOrder>,
    pub where_filter: Option<ChannelOverrideTemplateWhereInput>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelOverrideTemplateExtError {
    #[error("channel override template service is not available")]
    ServiceUnavailable,
    #[error("channel override template operation failed: {0}")]
    Operation(String),
}

#[async_trait::async_trait]
pub trait ChannelOverrideTemplateExtServices: Send + Sync {
    async fn list(
        &self,
        user_id: i64,
        args: ChannelOverrideTemplateConnectionArgs,
    ) -> Result<ChannelOverrideTemplateConnection, ChannelOverrideTemplateExtError>;
    async fn create(
        &self,
        user_id: i64,
        input: CreateChannelOverrideTemplateInput,
    ) -> Result<ChannelOverrideTemplate, ChannelOverrideTemplateExtError>;
    async fn update(
        &self,
        user_id: i64,
        id: String,
        input: UpdateChannelOverrideTemplateInput,
    ) -> Result<ChannelOverrideTemplate, ChannelOverrideTemplateExtError>;
    async fn delete(
        &self,
        user_id: i64,
        id: String,
    ) -> Result<bool, ChannelOverrideTemplateExtError>;
    async fn apply(
        &self,
        user_id: i64,
        input: ApplyChannelOverrideTemplateInput,
    ) -> Result<ApplyChannelOverrideTemplatePayload, ChannelOverrideTemplateExtError>;
    async fn clear(
        &self,
        input: ClearChannelOverrideTemplatesInput,
    ) -> Result<ClearChannelOverrideTemplatesPayload, ChannelOverrideTemplateExtError>;
}

pub(crate) fn channel_override_template_ext_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelOverrideTemplateExtServices>, String> {
    ctx.data::<Arc<dyn ChannelOverrideTemplateExtServices>>()
        .map(Arc::clone)
        .map_err(|_| ChannelOverrideTemplateExtError::ServiceUnavailable.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::me::CurrentUser;

    #[derive(Default)]
    struct FakeService {
        created: Mutex<Option<(i64, String)>>,
    }

    fn sample(name: &str) -> ChannelOverrideTemplate {
        let now = chrono::Utc::now();
        ChannelOverrideTemplate {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            user_id: Some(ID::from("7")),
            name: name.to_string(),
            description: None,
            override_parameters: "{}".to_string(),
            override_headers: Vec::new(),
            header_override_operations: Vec::new(),
            body_override_operations: vec![OverrideOperation {
                op: "set".to_string(),
                path: Some("temperature".to_string()),
                from: None,
                to: None,
                value: Some("0.8".to_string()),
                condition: None,
                match_: None,
                index: None,
                splat: None,
            }],
            user: None,
        }
    }

    #[async_trait::async_trait]
    impl ChannelOverrideTemplateExtServices for FakeService {
        async fn list(
            &self,
            _user_id: i64,
            _args: ChannelOverrideTemplateConnectionArgs,
        ) -> Result<ChannelOverrideTemplateConnection, ChannelOverrideTemplateExtError> {
            Ok(ChannelOverrideTemplateConnection {
                edges: Some(vec![Some(ChannelOverrideTemplateEdge {
                    node: Some(sample("template")),
                    cursor: CursorScalar("cursor".to_string()),
                })]),
                page_info: PageInfo::empty(false, false),
                total_count: 1,
            })
        }

        async fn create(
            &self,
            user_id: i64,
            input: CreateChannelOverrideTemplateInput,
        ) -> Result<ChannelOverrideTemplate, ChannelOverrideTemplateExtError> {
            *self.created.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((user_id, input.name.clone()));
            Ok(sample(&input.name))
        }

        async fn update(
            &self,
            _user_id: i64,
            _id: String,
            _input: UpdateChannelOverrideTemplateInput,
        ) -> Result<ChannelOverrideTemplate, ChannelOverrideTemplateExtError> {
            Err(ChannelOverrideTemplateExtError::Operation("unused".into()))
        }

        async fn delete(
            &self,
            _user_id: i64,
            _id: String,
        ) -> Result<bool, ChannelOverrideTemplateExtError> {
            Err(ChannelOverrideTemplateExtError::Operation("unused".into()))
        }

        async fn apply(
            &self,
            _user_id: i64,
            _input: ApplyChannelOverrideTemplateInput,
        ) -> Result<ApplyChannelOverrideTemplatePayload, ChannelOverrideTemplateExtError> {
            Err(ChannelOverrideTemplateExtError::Operation("unused".into()))
        }

        async fn clear(
            &self,
            _input: ClearChannelOverrideTemplatesInput,
        ) -> Result<ClearChannelOverrideTemplatesPayload, ChannelOverrideTemplateExtError> {
            Err(ChannelOverrideTemplateExtError::Operation("unused".into()))
        }
    }

    #[tokio::test]
    async fn query_returns_frontend_connection_shape() -> Result<(), Box<dyn std::error::Error>> {
        let service: Arc<dyn ChannelOverrideTemplateExtServices> = Arc::new(FakeService::default());
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute(
                async_graphql::Request::new(
                    "query { channelOverrideTemplates(first: 10) { totalCount edges { cursor node { id name bodyOverrideOperations { op path value } } } pageInfo { hasNextPage } } }",
                )
                .data(CurrentUser { user_id: 7 }),
            )
            .await;
        assert!(response.errors.is_empty(), "errors: {:?}", response.errors);
        let data = response.data.into_json()?;
        assert_eq!(data["channelOverrideTemplates"]["totalCount"], 1);
        assert_eq!(
            data["channelOverrideTemplates"]["edges"][0]["node"]["name"],
            "template"
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_uses_authenticated_user() -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeService::default());
        let service: Arc<dyn ChannelOverrideTemplateExtServices> = fake.clone();
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute(
                async_graphql::Request::new(
                    r#"mutation { createChannelOverrideTemplate(input: { name: "new" }) { id name userID } }"#,
                )
                .data(CurrentUser { user_id: 7 }),
            )
            .await;
        assert!(response.errors.is_empty(), "errors: {:?}", response.errors);
        let created = fake
            .created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(created, Some((7, "new".to_string())));
        Ok(())
    }
}

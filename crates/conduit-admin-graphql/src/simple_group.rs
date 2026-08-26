//! Simple-mode Group façade.
//!
//! This contract exposes the product concepts needed by the simple admin UI
//! while keeping Access Plan versions, routing and provider policy behind the
//! enterprise boundary.

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

use crate::scalars::TimeScalar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum SimpleGroupStatus {
    Enabled,
    Disabled,
    Archived,
}

/// Stable simple-mode projection of a customer commercial bundle.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct SimpleGroup {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub status: SimpleGroupStatus,
    pub is_default: bool,
    #[graphql(name = "accessPlanID")]
    pub access_plan_id: ID,
    #[graphql(name = "priceTierID")]
    pub price_tier_id: ID,
    #[graphql(name = "defaultSubscriptionPlanID")]
    pub default_subscription_plan_id: Option<ID>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<ID>,
    /// Optional enterprise-mode restriction to concrete SKU routes. Empty
    /// means every enabled route of each granted SKU may be used.
    #[graphql(name = "routeIDs")]
    pub route_ids: Vec<ID>,
    pub multiplier_ppm: i64,
    #[graphql(name = "memberUserIDs")]
    pub member_user_ids: Vec<ID>,
    #[graphql(name = "memberProjectIDs")]
    pub member_project_ids: Vec<ID>,
    pub unresolved_member_count: i32,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
}

/// A Project-scoped model-group choice for API-key policy editors. The
/// contained model/channel IDs are already restricted to that Project's
/// effective access, so the UI cannot offer another tenant's inventory.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct APIKeyAssignableGroup {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub status: SimpleGroupStatus,
    #[graphql(name = "allowedModelIDs")]
    pub allowed_model_ids: Vec<ID>,
    #[graphql(name = "allowedChannelIDs")]
    pub allowed_channel_ids: Vec<ID>,
}

/// Create one simple-mode commercial bundle.
///
/// Exactly one of `accessPlanID` and `modelIDs` must be provided, and exactly
/// one of `priceTierID` and `multiplierPpm` must be provided. Supplying IDs
/// links existing enterprise objects; supplying values creates them in the
/// same transaction as the Group.
#[derive(Debug, Clone, InputObject)]
pub struct CreateSimpleGroupInput {
    pub name: String,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    #[graphql(name = "accessPlanID")]
    pub access_plan_id: Option<ID>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<ID>>,
    #[graphql(name = "routeIDs")]
    pub route_ids: Option<Vec<ID>>,
    #[graphql(name = "priceTierID")]
    pub price_tier_id: Option<ID>,
    pub multiplier_ppm: Option<i64>,
    #[graphql(name = "defaultSubscriptionPlanID")]
    pub default_subscription_plan_id: Option<ID>,
    #[graphql(name = "userIDs")]
    pub user_ids: Option<Vec<ID>>,
}

/// Atomically update the simple-mode bundle and, when supplied, replace its
/// complete user set. Omitted fields retain their current value.
#[derive(Debug, Clone, InputObject)]
pub struct UpdateSimpleGroupInput {
    #[graphql(name = "groupID")]
    pub group_id: ID,
    pub name: Option<String>,
    pub description: Option<String>,
    pub clear_description: Option<bool>,
    pub status: Option<SimpleGroupStatus>,
    pub is_default: Option<bool>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<ID>>,
    #[graphql(name = "routeIDs")]
    pub route_ids: Option<Vec<ID>>,
    pub multiplier_ppm: Option<i64>,
    #[graphql(name = "defaultSubscriptionPlanID")]
    pub default_subscription_plan_id: Option<ID>,
    pub clear_default_subscription_plan: Option<bool>,
    #[graphql(name = "userIDs")]
    pub user_ids: Option<Vec<ID>>,
}

/// Assign users to a Simple Group through their strictly resolved personal
/// Projects. User IDs are accepted only as an admin-facing selection input;
/// the Group contract and persisted membership remain Project-scoped.
#[derive(Debug, Clone, InputObject)]
pub struct AssignSimpleGroupUsersInput {
    #[graphql(name = "groupID")]
    pub group_id: ID,
    #[graphql(name = "userIDs")]
    pub user_ids: Vec<ID>,
}

#[derive(Debug, Clone, InputObject)]
pub struct UpdateSimpleGroupModelsInput {
    #[graphql(name = "groupID")]
    pub group_id: ID,
    #[graphql(name = "modelIDs")]
    pub model_ids: Vec<ID>,
}

#[derive(Debug, Clone, InputObject)]
pub struct UpdateSimpleGroupPriceInput {
    #[graphql(name = "groupID")]
    pub group_id: ID,
    pub multiplier_ppm: i64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SimpleGroupServiceError {
    #[error("simple group service is unavailable")]
    Unavailable,
    #[error("simple group not found: {0}")]
    NotFound(String),
    #[error("invalid simple group input: {0}")]
    Invalid(String),
    #[error("simple group operation failed: {0}")]
    Storage(String),
}

#[async_trait::async_trait]
pub trait SimpleGroupServices: Send + Sync {
    async fn simple_groups(&self) -> Result<Vec<SimpleGroup>, SimpleGroupServiceError>;
    async fn api_key_assignable_groups(
        &self,
        project_id: i64,
    ) -> Result<Vec<APIKeyAssignableGroup>, SimpleGroupServiceError>;
    async fn create_simple_group(
        &self,
        actor_user_id: Option<i64>,
        input: CreateSimpleGroupInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
    async fn update_simple_group(
        &self,
        actor_user_id: Option<i64>,
        input: UpdateSimpleGroupInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
    async fn assign_simple_group_users(
        &self,
        actor_user_id: Option<i64>,
        input: AssignSimpleGroupUsersInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
    async fn update_simple_group_models(
        &self,
        input: UpdateSimpleGroupModelsInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
    async fn update_simple_group_price(
        &self,
        actor_user_id: Option<i64>,
        input: UpdateSimpleGroupPriceInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
    async fn delete_simple_group(
        &self,
        actor_user_id: Option<i64>,
        group_id: &str,
    ) -> Result<SimpleGroup, SimpleGroupServiceError>;
}

pub(crate) fn simple_group_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn SimpleGroupServices>, String> {
    ctx.data::<Arc<dyn SimpleGroupServices>>()
        .cloned()
        .map_err(|_| SimpleGroupServiceError::Unavailable.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use chrono::{TimeZone, Utc};

    use crate::QueryRoot;

    struct Stub;

    #[async_trait::async_trait]
    impl SimpleGroupServices for Stub {
        async fn simple_groups(&self) -> Result<Vec<SimpleGroup>, SimpleGroupServiceError> {
            let at = TimeScalar(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
            Ok(vec![SimpleGroup {
                id: ID("7".into()),
                name: "Starter".into(),
                description: None,
                status: SimpleGroupStatus::Enabled,
                is_default: true,
                access_plan_id: ID("11".into()),
                price_tier_id: ID("12".into()),
                default_subscription_plan_id: Some(ID("3".into())),
                model_ids: vec![ID("1".into())],
                route_ids: vec![ID("8".into())],
                multiplier_ppm: 1_000_000,
                member_user_ids: vec![ID("7".into())],
                member_project_ids: vec![ID("9".into())],
                unresolved_member_count: 0,
                created_at: at.clone(),
                updated_at: at,
            }])
        }

        async fn api_key_assignable_groups(
            &self,
            _project_id: i64,
        ) -> Result<Vec<APIKeyAssignableGroup>, SimpleGroupServiceError> {
            Ok(vec![APIKeyAssignableGroup {
                id: ID("7".into()),
                name: "Starter".into(),
                description: None,
                status: SimpleGroupStatus::Enabled,
                allowed_model_ids: vec![ID("gpt-5".into())],
                allowed_channel_ids: vec![ID("3".into())],
            }])
        }

        async fn create_simple_group(
            &self,
            _actor_user_id: Option<i64>,
            input: CreateSimpleGroupInput,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            let at = TimeScalar(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
            Ok(SimpleGroup {
                id: ID("018f0f43-d45f-7b39-a9c4-0a3c332e6c21".into()),
                name: input.name,
                description: input.description,
                status: SimpleGroupStatus::Enabled,
                is_default: input.is_default.unwrap_or(false),
                access_plan_id: input.access_plan_id.unwrap_or_else(|| ID("11".into())),
                price_tier_id: input.price_tier_id.unwrap_or_else(|| ID("12".into())),
                default_subscription_plan_id: input.default_subscription_plan_id,
                model_ids: input.model_ids.unwrap_or_default(),
                route_ids: input.route_ids.unwrap_or_default(),
                multiplier_ppm: input.multiplier_ppm.unwrap_or(1_000_000),
                member_user_ids: input.user_ids.unwrap_or_default(),
                member_project_ids: Vec::new(),
                unresolved_member_count: 0,
                created_at: at.clone(),
                updated_at: at,
            })
        }

        async fn update_simple_group(
            &self,
            _actor_user_id: Option<i64>,
            input: UpdateSimpleGroupInput,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            let mut group = simple_group(input.group_id)?;
            if let Some(name) = input.name {
                group.name = name;
            }
            if let Some(model_ids) = input.model_ids {
                group.model_ids = model_ids;
            }
            if let Some(route_ids) = input.route_ids {
                group.route_ids = route_ids;
            }
            if let Some(multiplier_ppm) = input.multiplier_ppm {
                group.multiplier_ppm = multiplier_ppm;
            }
            if let Some(user_ids) = input.user_ids {
                group.member_user_ids = user_ids;
            }
            Ok(group)
        }

        async fn assign_simple_group_users(
            &self,
            _actor_user_id: Option<i64>,
            input: AssignSimpleGroupUsersInput,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            let at = TimeScalar(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
            Ok(SimpleGroup {
                id: input.group_id,
                name: "Starter".into(),
                description: None,
                status: SimpleGroupStatus::Enabled,
                is_default: false,
                access_plan_id: ID("11".into()),
                price_tier_id: ID("12".into()),
                default_subscription_plan_id: None,
                model_ids: Vec::new(),
                route_ids: Vec::new(),
                multiplier_ppm: 1_000_000,
                member_user_ids: vec![ID("7".into())],
                member_project_ids: vec![ID("9".into())],
                unresolved_member_count: 0,
                created_at: at.clone(),
                updated_at: at,
            })
        }

        async fn update_simple_group_models(
            &self,
            input: UpdateSimpleGroupModelsInput,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            simple_group(input.group_id)
        }

        async fn update_simple_group_price(
            &self,
            _actor_user_id: Option<i64>,
            input: UpdateSimpleGroupPriceInput,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            simple_group(input.group_id)
        }

        async fn delete_simple_group(
            &self,
            _actor_user_id: Option<i64>,
            group_id: &str,
        ) -> Result<SimpleGroup, SimpleGroupServiceError> {
            let mut group = simple_group(ID(group_id.into()))?;
            group.status = SimpleGroupStatus::Archived;
            Ok(group)
        }
    }

    fn simple_group(id: ID) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let at = TimeScalar(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
        Ok(SimpleGroup {
            id,
            name: "Starter".into(),
            description: None,
            status: SimpleGroupStatus::Enabled,
            is_default: false,
            access_plan_id: ID("11".into()),
            price_tier_id: ID("12".into()),
            default_subscription_plan_id: None,
            model_ids: vec![ID("1".into())],
            route_ids: vec![ID("8".into())],
            multiplier_ppm: 1_000_000,
            member_user_ids: Vec::new(),
            member_project_ids: Vec::new(),
            unresolved_member_count: 0,
            created_at: at.clone(),
            updated_at: at,
        })
    }

    fn schema() -> Schema<QueryRoot, EmptyMutation, EmptySubscription> {
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish()
    }

    #[tokio::test]
    async fn query_exposes_simple_facade_without_enterprise_policy_fields() {
        let schema = schema();
        let response = schema
            .execute(Request::new(
                "{ simpleGroups { id name accessPlanID priceTierID defaultSubscriptionPlanID modelIDs routeIDs multiplierPpm memberUserIDs memberProjectIDs unresolvedMemberCount } }",
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["simpleGroups"][0]["id"], "7");
        assert_eq!(data["simpleGroups"][0]["memberProjectIDs"][0], "9");
        assert_eq!(data["simpleGroups"][0]["modelIDs"][0], "1");
        assert_eq!(data["simpleGroups"][0]["routeIDs"][0], "8");

        let sdl = schema.sdl();
        let group = sdl
            .split("type SimpleGroup {")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("SimpleGroup SDL block");
        assert!(!group.contains("allowedModelIDs"));
        assert!(group.contains("routeIDs"));
        assert!(group.contains("memberUserIDs"));
    }

    #[tokio::test]
    async fn mutation_exposes_create_or_link_contract() {
        let schema = Schema::build(QueryRoot, crate::mutation::MutationRoot, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish();
        let response = schema
            .execute(Request::new(
                r#"mutation {
                    createSimpleGroup(input: {
                        name: "Starter"
                        modelIDs: ["1", "2"]
                        routeIDs: ["8"]
                        multiplierPpm: 1000000
                    }) {
                        name accessPlanID priceTierID modelIDs routeIDs multiplierPpm memberProjectIDs
                    }
                }"#,
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["createSimpleGroup"]["name"], "Starter");
        assert_eq!(data["createSimpleGroup"]["accessPlanID"], "11");
        assert_eq!(data["createSimpleGroup"]["priceTierID"], "12");
        assert_eq!(data["createSimpleGroup"]["multiplierPpm"], 1000000);
        assert_eq!(data["createSimpleGroup"]["routeIDs"][0], "8");
    }

    #[tokio::test]
    async fn mutation_exposes_atomic_simple_group_update() {
        let schema = Schema::build(QueryRoot, crate::mutation::MutationRoot, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish();
        let response = schema
            .execute(Request::new(
                r#"mutation {
                    updateSimpleGroup(input: {
                        groupID: "018f0f43-d45f-7b39-a9c4-0a3c332e6c21"
                        name: "Pro"
                        modelIDs: ["2"]
                        routeIDs: ["9"]
                        multiplierPpm: 1250000
                        userIDs: ["7"]
                    }) { name modelIDs routeIDs multiplierPpm memberUserIDs }
                }"#,
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["updateSimpleGroup"]["name"], "Pro");
        assert_eq!(data["updateSimpleGroup"]["modelIDs"][0], "2");
        assert_eq!(data["updateSimpleGroup"]["routeIDs"][0], "9");
        assert_eq!(data["updateSimpleGroup"]["multiplierPpm"], 1250000);
        assert_eq!(data["updateSimpleGroup"]["memberUserIDs"][0], "7");
    }

    #[tokio::test]
    async fn mutation_assigns_users_without_exposing_user_membership() {
        let schema = Schema::build(QueryRoot, crate::mutation::MutationRoot, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish();
        let response = schema
            .execute(Request::new(
                r#"mutation {
                    assignSimpleGroupUsers(input: {
                        groupID: "018f0f43-d45f-7b39-a9c4-0a3c332e6c21"
                        userIDs: ["7"]
                    }) { id memberProjectIDs }
                }"#,
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["assignSimpleGroupUsers"]["memberProjectIDs"][0], "9");
        let sdl = schema.sdl();
        let simple_group = sdl
            .split("type SimpleGroup {")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("SimpleGroup SDL block");
        assert!(simple_group.contains("memberUserIDs"));
    }

    #[tokio::test]
    async fn mutations_expose_model_and_retail_price_edits() {
        let schema = Schema::build(QueryRoot, crate::mutation::MutationRoot, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish();
        let response = schema
            .execute(Request::new(
                r#"mutation {
                    models: updateSimpleGroupModels(input: {
                        groupID: "018f0f43-d45f-7b39-a9c4-0a3c332e6c21"
                        modelIDs: ["1", "2"]
                    }) { accessPlanID }
                    price: updateSimpleGroupPrice(input: {
                        groupID: "018f0f43-d45f-7b39-a9c4-0a3c332e6c21"
                        multiplierPpm: 1250000
                    }) { priceTierID }
                }"#,
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["models"]["accessPlanID"], "11");
        assert_eq!(data["price"]["priceTierID"], "12");
    }

    #[tokio::test]
    async fn delete_mutation_exposes_archive_semantics_without_a_hard_delete_switch() {
        let schema = Schema::build(QueryRoot, crate::mutation::MutationRoot, EmptySubscription)
            .data(Arc::new(Stub) as Arc<dyn SimpleGroupServices>)
            .finish();
        let response = schema
            .execute(Request::new(
                r#"mutation {
                    deleteSimpleGroup(id: "018f0f43-d45f-7b39-a9c4-0a3c332e6c21") {
                        id status
                    }
                }"#,
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["deleteSimpleGroup"]["status"], "ARCHIVED");
        assert!(!schema.sdl().contains("hardDelete"));
        assert!(!schema.sdl().contains("purgeSimpleGroup"));
    }
}

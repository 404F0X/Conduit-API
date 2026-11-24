//! Fixed command DTOs and PostgreSQL transaction appliers. No resolver replay
//! or caller-selected SQL/column names are accepted.
use crate::wiring_governance::{self as policy, Policy};
use crate::wiring_governance_inputs::GraphInput;
use conduit_admin_graphql::{change_package as package, governance as gql};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

macro_rules! record {
    ($name:ident {$($field:ident:$ty:ty),* $(,)?}) => {
        #[derive(Clone,Serialize,Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $name { $(#[serde(skip_serializing_if="Option::is_none")] $field:Option<$ty>),* }
    };
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GovernedCredentials {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gcp: Option<conduit_core::objects::channel_settings::GCPCredential>,
    #[serde(skip_serializing_if = "Option::is_none")]
    azure: Option<conduit_core::objects::channel_settings::AzureCredential>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_api_pat: Option<String>,
    #[serde(rename = "newApiUserID", skip_serializing_if = "Option::is_none")]
    new_api_user_id: Option<String>,
}
record!(ChannelWrite {name:String,r#type:String,base_url:String,default_test_model:String,ordering_weight:i32,auto_sync_supported_models:bool,auto_sync_model_pattern:String,supported_models:Vec<String>,manual_models:Vec<String>,tags:Vec<String>,settings:conduit_core::objects::channel_settings::ChannelSettings,credentials:GovernedCredentials,policies:conduit_core::objects::channel_settings::ChannelPolicies,endpoints:Vec<conduit_core::objects::channel_settings::ChannelEndpoint>,remark:String,status:String});
record!(ModelWrite {
    model_id: String,
    developer: String,
    r#type: String,
    name: String,
    r#group: String,
    icon: String,
    status: String,
    model_card: conduit_core::objects::model::ModelCard,
    settings: conduit_core::objects::model::ModelSettings,
    remark: String
});
record!(ProjectWrite {
    name: String,
    description: String,
    status: String,
    profiles: conduit_core::objects::project::ProjectProfiles
});
record!(UserWrite {email:String,first_name:String,last_name:String,prefer_language:String,scopes:Vec<String>,status:String,password:String});
record!(RoleWrite {name:String,level:String,project_id:i64,scopes:Vec<String>});
record!(MemberWrite {user_id:i64,project_id:i64,is_owner:bool,scopes:Vec<String>});
record!(KeyWrite {user_id:i64,project_id:i64,name:String,r#type:String,status:String,scopes:Vec<String>,profiles:conduit_core::objects::apikey::APIKeyProfiles});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleAssignment {
    user_id: i64,
    role_ids: Vec<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteWrite {
    public_model_id: i64,
    deployment_id: i64,
    status: String,
    #[serde(default)]
    confirm_compatibility: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentWrite {
    channel_id: i64,
    upstream_model_id: String,
    internal_name: String,
    variant: String,
    status: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionAction {
    auto_renew: Option<bool>,
}
record!(CommercialWrite {
    account_type: String,
    owner_user_id: i64,
    base_access_plan_id: i64,
    base_price_tier_id: i64,
    status: String
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpendLimit {
    project_id: i64,
    user_id: i64,
    limit: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingWrite {
    key: String,
    value: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplyWrite {
    supported_models: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteCommand {}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelBatch {
    channels: Vec<ChannelWrite>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelBatch {
    models: Vec<ModelWrite>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicateChannel {
    name: String,
    include_prices: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelBatchForm {
    models: Vec<GraphInput<conduit_admin_graphql::model::CreateModelInput>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelKeyAction {
    operation: ChannelKeyOperation,
    keys: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChannelKeyOperation {
    Disable,
    Enable,
    EnableAll,
    DeleteDisabled,
}
record!(OverrideTemplateWrite {name:String,description:String,header_override_operations:Vec<conduit_core::objects::overrides::OverrideOperation>,body_override_operations:Vec<conduit_core::objects::overrides::OverrideOperation>});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyTemplate {
    template_id: i64,
    replace: bool,
}
record!(KeyTemplateWrite {
    project_id: i64,
    name: String,
    description: String,
    profile: conduit_core::objects::apikey::APIKeyProfile
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadKeyTemplate {
    template_id: i64,
}
record!(ProtectionWrite {
    name: String,
    description: String,
    pattern: String,
    status: String,
    settings: conduit_core::objects::prompt_protection::PromptProtectionSettings
});
record!(StorageWrite {
    name: String,
    description: String,
    r#type: String,
    settings: conduit_core::objects::storage::DataStorageSettings,
    status: String
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicModelRoutes {
    model: ModelWrite,
    deployment_ids: Vec<i64>,
    #[serde(default)]
    confirm_compatibility: bool,
    #[serde(default = "enabled_default")]
    enabled: bool,
}
fn enabled_default() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyTemplateCreateForm {
    input: GraphInput<conduit_admin_graphql::profile_template::CreateAPIKeyProfileTemplateInput>,
    profile: GraphInput<conduit_admin_graphql::apikey::APIKeyProfileInput>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyTemplateUpdateForm {
    input: GraphInput<conduit_admin_graphql::profile_template::UpdateAPIKeyProfileTemplateInput>,
    profile: Option<GraphInput<conduit_admin_graphql::apikey::APIKeyProfileInput>>,
}
record!(AccessPlanWrite {
    name: String,
    description: String,
    status: String,
    is_default: bool
});
record!(PriceTierWrite {
    name: String,
    multiplier_ppm: i64,
    status: String,
    is_default: bool
});
record!(GrantWrite {project_id:i64,access_plan_version_id:i64,source_type:String,source_id:String,status:String,valid_from:chrono::DateTime<chrono::Utc>,valid_until:chrono::DateTime<chrono::Utc>});
record!(EntitlementWrite {project_id:i64,public_model_id:i64,effect:String,source_type:String,source_id:String,status:String,valid_from:chrono::DateTime<chrono::Utc>,valid_until:chrono::DateTime<chrono::Utc>,reason:String});
record!(AdjustmentWrite {project_id:i64,multiplier_ppm:i64,stacking_key:String,priority:i32,source_type:String,source_id:String,status:String,valid_from:chrono::DateTime<chrono::Utc>,valid_until:chrono::DateTime<chrono::Utc>});
#[derive(Clone, Serialize)]
enum Command {
    ChannelDuplicateForm(GraphInput<conduit_admin_graphql::channel::CreateChannelInput>),ChannelEndpointsForm(GraphInput<conduit_admin_graphql::channel_ext::SaveChannelEndpointsInput>),ChannelOrderingForm(GraphInput<conduit_admin_graphql::channel_ext2::BulkUpdateChannelOrderingInput>),ModelBatchForm(ModelBatchForm),
    MappingAutomation(GraphInput<conduit_admin_graphql::commercialization::SetChannelModelMappingAutomationInput>),AliasesForm(GraphInput<conduit_admin_graphql::commercialization::ApplyChannelModelMappingsInput>),
    RouteForm(GraphInput<conduit_admin_graphql::commercialization::UpsertModelRouteInput>),PublicModelForm(GraphInput<conduit_admin_graphql::commercialization::CreatePublicModelWithRoutesInput>),PriceBookForm(GraphInput<conduit_admin_graphql::commercialization::CreatePriceBookInput>),
    SettingForm(crate::wiring_governance_settings::SettingForm),
    ProjectProfilesForm(GraphInput<conduit_admin_graphql::project::UpdateProjectProfilesInput>),KeyTemplateCreateForm(KeyTemplateCreateForm),KeyTemplateUpdateForm(KeyTemplateUpdateForm),
    ChannelTemplateCreateForm(GraphInput<conduit_admin_graphql::channel_override_template_ext::CreateChannelOverrideTemplateInput>),ChannelTemplateUpdateForm(GraphInput<conduit_admin_graphql::channel_override_template_ext::UpdateChannelOverrideTemplateInput>),
    KeyCreateForm(GraphInput<conduit_admin_graphql::apikey::CreateAPIKeyInput>),KeyUpdateForm(GraphInput<conduit_admin_graphql::apikey::UpdateAPIKeyInput>),KeyProfilesForm(GraphInput<conduit_admin_graphql::apikey::UpdateAPIKeyProfilesInput>),
    StorageCreateForm(GraphInput<conduit_admin_graphql::data_storage::CreateDataStorageInput>),StorageUpdateForm(GraphInput<conduit_admin_graphql::data_storage::UpdateDataStorageInput>),
    ProtectionCreateForm(GraphInput<conduit_admin_graphql::prompt::CreatePromptProtectionRuleInput>),ProtectionUpdateForm(GraphInput<conduit_admin_graphql::prompt::UpdatePromptProtectionRuleInput>),
    MemberAddForm(GraphInput<conduit_admin_graphql::user::AddUserToProjectInput>),MemberUpdateForm(GraphInput<conduit_admin_graphql::user::UpdateProjectUserInput>),MemberRemoveForm(GraphInput<conduit_admin_graphql::user::RemoveUserFromProjectInput>),
    ModelCreateForm(GraphInput<conduit_admin_graphql::model::CreateModelInput>),ModelUpdateForm(GraphInput<conduit_admin_graphql::model::UpdateModelInput>),
    UserCreateForm(GraphInput<conduit_admin_graphql::user::CreateUserInput>),UserUpdateForm(GraphInput<conduit_admin_graphql::user::UpdateUserInput>),
    ProjectCreateForm(GraphInput<conduit_admin_graphql::project::CreateProjectInput>),ProjectUpdateForm(GraphInput<conduit_admin_graphql::project::UpdateProjectInput>),
    RoleCreateForm(GraphInput<conduit_admin_graphql::role::CreateRoleInput>),RoleUpdateForm(GraphInput<conduit_admin_graphql::role::UpdateRoleInput>),
    ChannelBulkCreateForm(GraphInput<conduit_admin_graphql::channel_ext2::BulkCreateChannelsInput>),ChannelImportForm(GraphInput<conduit_admin_graphql::channel_ext2::BulkImportChannelsInput>),
    ChannelCreateForm(GraphInput<conduit_admin_graphql::channel::CreateChannelInput>),ChannelUpdateForm(GraphInput<conduit_admin_graphql::channel::UpdateChannelInput>),
    ChannelBatch(ChannelBatch),ModelBatch(ModelBatch),ChannelDuplicate(DuplicateChannel),ChannelKeys(ChannelKeyAction),OverrideTemplate(OverrideTemplateWrite),OverrideTemplateDelete(DeleteCommand),TemplateApply(ApplyTemplate),TemplateClear(DeleteCommand),KeyTemplate(KeyTemplateWrite),KeyTemplateDelete(DeleteCommand),KeyTemplateLoad(LoadKeyTemplate),Protection(ProtectionWrite),ProtectionDelete(DeleteCommand),Storage(StorageWrite),PublicModelRoutes(PublicModelRoutes),AccessPlan(AccessPlanWrite),PriceTier(PriceTierWrite),Grant(GrantWrite),Entitlement(EntitlementWrite),Adjustment(AdjustmentWrite),
    Channel(ChannelWrite),ChannelDelete(DeleteCommand),Model(ModelWrite),ModelDelete(DeleteCommand),Supply(SupplyWrite),
    Project(ProjectWrite),ProjectDelete(DeleteCommand),User(UserWrite),UserDelete(DeleteCommand),Role(RoleWrite),RoleDelete(DeleteCommand),Member(MemberWrite),MemberRemove(MemberWrite),
    SpendLimit(SpendLimit),Setting(SettingWrite),Key(KeyWrite),KeyRotate(DeleteCommand),KeyDelete(DeleteCommand),Roles(RoleAssignment),Route(RouteWrite),Deployment(DeploymentWrite),Commercial(CommercialWrite),
    GroupCreate(conduit_admin_graphql::simple_group::CreateSimpleGroupInput),GroupUpdate(conduit_admin_graphql::simple_group::UpdateSimpleGroupInput),GroupAssign(conduit_admin_graphql::simple_group::AssignSimpleGroupUsersInput),GroupModels(conduit_admin_graphql::simple_group::UpdateSimpleGroupModelsInput),GroupPrice(conduit_admin_graphql::simple_group::UpdateSimpleGroupPriceInput),GroupDelete(DeleteCommand),
    Credit(conduit_admin_graphql::billing::GrantProjectCreditInput),PlanCreate(conduit_admin_graphql::billing::CreateSubscriptionPlanInput),PlanUpdate(conduit_admin_graphql::billing::UpdateSubscriptionPlanInput),SubscriptionAssign(conduit_admin_graphql::billing::AssignUserSubscriptionInput),SubscriptionCancel(DeleteCommand),SubscriptionRefresh(DeleteCommand),SubscriptionPause(DeleteCommand),SubscriptionResume(DeleteCommand),SubscriptionRenew(DeleteCommand),SubscriptionAutoRenew(SubscriptionAction),RedemptionCreate(conduit_admin_graphql::billing::CreateCreditRedemptionCodesInput),RedemptionRevoke(DeleteCommand),
}
const ACTIONS: &[(&str, &str, &str)] = &[
    (
        "channel.duplicate_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel.endpoints_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel.ordering_form",
        "channel_configuration",
        "write_channels",
    ),
    ("model.batch_form", "model_supply", "write_channels"),
    (
        "model.mapping_automation",
        "model_mapping",
        "write_commercialization",
    ),
    (
        "channel.aliases_form",
        "model_mapping",
        "write_commercialization",
    ),
    (
        "model.route_form",
        "model_supply",
        "write_commercialization",
    ),
    (
        "model.public_form",
        "model_supply",
        "write_commercialization",
    ),
    (
        "price_book.create_form",
        "retail_price",
        "write_commercialization",
    ),
    (
        "system.product_experience_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.brand_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.general_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.model_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.channel_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.security_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.storage_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.retry_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.webhook_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.video_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.backup_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.quota_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.proxy_save",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.proxy_delete",
        "system_configuration",
        "write_settings",
    ),
    (
        "system.public_health_form",
        "system_configuration",
        "write_settings",
    ),
    (
        "project.profiles_form",
        "project_configuration",
        "write_projects",
    ),
    (
        "key_template.create_form",
        "identity_access",
        "write_api_keys",
    ),
    (
        "key_template.update_form",
        "identity_access",
        "write_api_keys",
    ),
    (
        "channel_template.create_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel_template.update_form",
        "channel_configuration",
        "write_channels",
    ),
    ("api_key.create_form", "identity_access", "write_api_keys"),
    ("api_key.update_form", "identity_access", "write_api_keys"),
    ("api_key.profiles_form", "identity_access", "write_api_keys"),
    (
        "storage.create_form",
        "system_configuration",
        "write_data_storages",
    ),
    (
        "storage.update_form",
        "system_configuration",
        "write_data_storages",
    ),
    (
        "protection.create_form",
        "system_configuration",
        "write_channels",
    ),
    (
        "protection.update_form",
        "system_configuration",
        "write_channels",
    ),
    ("member.add_form", "identity_access", "write_users"),
    ("member.update_form", "identity_access", "write_users"),
    ("member.remove_form", "identity_access", "write_users"),
    ("model.create_form", "model_supply", "write_channels"),
    ("model.update_form", "model_supply", "write_channels"),
    ("user.create_form", "identity_access", "write_users"),
    ("user.update_form", "identity_access", "write_users"),
    (
        "project.create_form",
        "project_configuration",
        "write_projects",
    ),
    (
        "project.update_form",
        "project_configuration",
        "write_projects",
    ),
    ("role.create_form", "identity_access", "write_roles"),
    ("role.update_form", "identity_access", "write_roles"),
    (
        "channel.bulk_create_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel.import_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel.create_form",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel.update_form",
        "channel_configuration",
        "write_channels",
    ),
    ("channel.batch", "channel_configuration", "write_channels"),
    (
        "channel.duplicate",
        "channel_configuration",
        "write_channels",
    ),
    ("channel.keys", "channel_configuration", "write_channels"),
    (
        "channel_template.write",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel_template.delete",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel_template.apply",
        "channel_configuration",
        "write_channels",
    ),
    (
        "channel_template.clear",
        "channel_configuration",
        "write_channels",
    ),
    ("model.batch", "model_supply", "write_channels"),
    ("model.create_with_routes", "model_supply", "write_channels"),
    ("key_template.write", "identity_access", "write_api_keys"),
    ("key_template.delete", "identity_access", "write_api_keys"),
    ("key_template.load", "identity_access", "write_api_keys"),
    ("protection.write", "system_configuration", "write_channels"),
    (
        "protection.delete",
        "system_configuration",
        "write_channels",
    ),
    (
        "storage.write",
        "system_configuration",
        "write_data_storages",
    ),
    (
        "access_plan.write",
        "project_configuration",
        "write_commercialization",
    ),
    (
        "price_tier.write",
        "retail_price",
        "write_commercialization",
    ),
    (
        "project.grant",
        "project_configuration",
        "write_commercialization",
    ),
    (
        "project.entitlement",
        "project_configuration",
        "write_commercialization",
    ),
    (
        "project.adjustment",
        "retail_price",
        "write_commercialization",
    ),
    ("channel.write", "channel_configuration", "write_channels"),
    ("channel.delete", "channel_configuration", "write_channels"),
    ("model.write", "model_supply", "write_channels"),
    ("model.delete", "model_supply", "write_channels"),
    ("model.supply", "model_supply", "write_channels"),
    ("project.write", "project_configuration", "write_projects"),
    ("project.delete", "project_configuration", "write_projects"),
    (
        "project.member_limit",
        "project_configuration",
        "write_billing",
    ),
    ("user.write", "identity_access", "write_users"),
    ("user.delete", "identity_access", "write_users"),
    ("role.write", "identity_access", "write_roles"),
    ("role.delete", "identity_access", "write_roles"),
    ("member.write", "identity_access", "write_users"),
    ("member.remove", "identity_access", "write_users"),
    ("group.create", "group_access", "write_groups"),
    ("group.update", "group_access", "write_groups"),
    ("group.assign", "group_access", "write_groups"),
    ("group.models", "group_access", "write_groups"),
    ("group.price", "group_access", "write_groups"),
    ("group.delete", "group_access", "write_groups"),
    ("system.setting", "system_configuration", "write_settings"),
    (
        "subscription_plan.create",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription_plan.update",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.assign",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.cancel",
        "system_configuration",
        "write_subscriptions",
    ),
    ("project.credit", "project_credit", "grant_credit"),
    (
        "subscription.refresh",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.pause",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.resume",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.renew",
        "system_configuration",
        "write_subscriptions",
    ),
    (
        "subscription.auto_renew",
        "system_configuration",
        "write_subscriptions",
    ),
    ("redemption.create", "system_configuration", "grant_credit"),
    ("redemption.revoke", "system_configuration", "grant_credit"),
    ("api_key.write", "identity_access", "write_api_keys"),
    ("api_key.rotate", "identity_access", "write_api_keys"),
    ("api_key.delete", "identity_access", "write_api_keys"),
    ("user.roles", "identity_access", "write_users"),
    ("model.route", "model_supply", "write_channels"),
    ("model.deployment", "model_supply", "write_channels"),
    (
        "project.commercial",
        "project_configuration",
        "write_commercialization",
    ),
];
pub(crate) fn capabilities() -> Result<Vec<gql::GovernedActionCapability>, String> {
    ACTIONS
        .iter()
        .map(|(a, k, s)| {
            Ok(gql::GovernedActionCapability {
                action: (*a).into(),
                kind: policy::kind(k)?,
                domain_scope: (*s).into(),
                supported: true,
            })
        })
        .collect()
}
fn registration(action: &str) -> Result<(&'static str, &'static str), String> {
    ACTIONS
        .iter()
        .find(|(a, _, _)| *a == action)
        .map(|(_, k, s)| (*k, *s))
        .ok_or_else(|| "GOVERNANCE_UNSUPPORTED: unknown action".into())
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|_| "VALIDATION_FAILED: command payload does not match its fixed DTO".into())
}
fn command(action: &str, payload: Value) -> Result<Command, String> {
    Ok(match action {
        "channel.duplicate_form" => Command::ChannelDuplicateForm(decode(payload)?),
        "channel.endpoints_form" => Command::ChannelEndpointsForm(decode(payload)?),
        "channel.ordering_form" => Command::ChannelOrderingForm(decode(payload)?),
        "model.batch_form" => Command::ModelBatchForm(decode(payload)?),
        "model.mapping_automation" => Command::MappingAutomation(decode(payload)?),
        "channel.aliases_form" => Command::AliasesForm(decode(payload)?),
        "model.route_form" => Command::RouteForm(decode(payload)?),
        "model.public_form" => Command::PublicModelForm(decode(payload)?),
        "price_book.create_form" => Command::PriceBookForm(decode(payload)?),
        "system.brand_form"
        | "system.product_experience_form"
        | "system.general_form"
        | "system.model_form"
        | "system.channel_form"
        | "system.security_form"
        | "system.storage_form"
        | "system.retry_form"
        | "system.webhook_form"
        | "system.video_form"
        | "system.backup_form"
        | "system.quota_form"
        | "system.proxy_save"
        | "system.proxy_delete"
        | "system.public_health_form" => Command::SettingForm(
            crate::wiring_governance_settings::SettingForm::parse(action, payload)?,
        ),
        "project.profiles_form" => Command::ProjectProfilesForm(decode(payload)?),
        "key_template.create_form" => Command::KeyTemplateCreateForm(decode(payload)?),
        "key_template.update_form" => Command::KeyTemplateUpdateForm(decode(payload)?),
        "channel_template.create_form" => Command::ChannelTemplateCreateForm(decode(payload)?),
        "channel_template.update_form" => Command::ChannelTemplateUpdateForm(decode(payload)?),
        "api_key.create_form" => Command::KeyCreateForm(decode(payload)?),
        "api_key.update_form" => Command::KeyUpdateForm(decode(payload)?),
        "api_key.profiles_form" => Command::KeyProfilesForm(decode(payload)?),
        "storage.create_form" => Command::StorageCreateForm(decode(payload)?),
        "storage.update_form" => Command::StorageUpdateForm(decode(payload)?),
        "protection.create_form" => Command::ProtectionCreateForm(decode(payload)?),
        "protection.update_form" => Command::ProtectionUpdateForm(decode(payload)?),
        "member.add_form" => Command::MemberAddForm(decode(payload)?),
        "member.update_form" => Command::MemberUpdateForm(decode(payload)?),
        "member.remove_form" => Command::MemberRemoveForm(decode(payload)?),
        "model.create_form" => Command::ModelCreateForm(decode(payload)?),
        "model.update_form" => Command::ModelUpdateForm(decode(payload)?),
        "user.create_form" => Command::UserCreateForm(decode(payload)?),
        "user.update_form" => Command::UserUpdateForm(decode(payload)?),
        "project.create_form" => Command::ProjectCreateForm(decode(payload)?),
        "project.update_form" => Command::ProjectUpdateForm(decode(payload)?),
        "role.create_form" => Command::RoleCreateForm(decode(payload)?),
        "role.update_form" => Command::RoleUpdateForm(decode(payload)?),
        "channel.bulk_create_form" => Command::ChannelBulkCreateForm(decode(payload)?),
        "channel.import_form" => Command::ChannelImportForm(decode(payload)?),
        "channel.create_form" => Command::ChannelCreateForm(decode(payload)?),
        "channel.update_form" => Command::ChannelUpdateForm(decode(payload)?),
        "channel.batch" => Command::ChannelBatch(decode(payload)?),
        "model.batch" => Command::ModelBatch(decode(payload)?),
        "channel.duplicate" => Command::ChannelDuplicate(decode(payload)?),
        "channel.keys" => Command::ChannelKeys(decode(payload)?),
        "channel_template.write" => Command::OverrideTemplate(decode(payload)?),
        "channel_template.delete" => Command::OverrideTemplateDelete(decode(payload)?),
        "channel_template.apply" => Command::TemplateApply(decode(payload)?),
        "channel_template.clear" => Command::TemplateClear(decode(payload)?),
        "key_template.write" => Command::KeyTemplate(decode(payload)?),
        "key_template.delete" => Command::KeyTemplateDelete(decode(payload)?),
        "key_template.load" => Command::KeyTemplateLoad(decode(payload)?),
        "protection.write" => Command::Protection(decode(payload)?),
        "protection.delete" => Command::ProtectionDelete(decode(payload)?),
        "storage.write" => Command::Storage(decode(payload)?),
        "model.create_with_routes" => Command::PublicModelRoutes(decode(payload)?),
        "access_plan.write" => Command::AccessPlan(decode(payload)?),
        "price_tier.write" => Command::PriceTier(decode(payload)?),
        "project.grant" => Command::Grant(decode(payload)?),
        "project.entitlement" => Command::Entitlement(decode(payload)?),
        "project.adjustment" => Command::Adjustment(decode(payload)?),
        "channel.write" => Command::Channel(decode(payload)?),
        "channel.delete" => Command::ChannelDelete(decode(payload)?),
        "model.write" => Command::Model(decode(payload)?),
        "model.delete" => Command::ModelDelete(decode(payload)?),
        "model.supply" => Command::Supply(decode(payload)?),
        "project.write" => Command::Project(decode(payload)?),
        "project.delete" => Command::ProjectDelete(decode(payload)?),
        "project.member_limit" => Command::SpendLimit(decode(payload)?),
        "user.write" => Command::User(decode(payload)?),
        "user.delete" => Command::UserDelete(decode(payload)?),
        "role.write" => Command::Role(decode(payload)?),
        "role.delete" => Command::RoleDelete(decode(payload)?),
        "member.write" => Command::Member(decode(payload)?),
        "member.remove" => Command::MemberRemove(decode(payload)?),
        "group.create" => Command::GroupCreate(decode(payload)?),
        "group.update" => Command::GroupUpdate(decode(payload)?),
        "group.assign" => Command::GroupAssign(decode(payload)?),
        "group.models" => Command::GroupModels(decode(payload)?),
        "group.price" => Command::GroupPrice(decode(payload)?),
        "group.delete" => Command::GroupDelete(decode(payload)?),
        "system.setting" => Command::Setting(decode(payload)?),
        "project.credit" => Command::Credit(decode(payload)?),
        "subscription_plan.create" => Command::PlanCreate(decode(payload)?),
        "subscription_plan.update" => Command::PlanUpdate(decode(payload)?),
        "subscription.assign" => Command::SubscriptionAssign(decode(payload)?),
        "subscription.cancel" => Command::SubscriptionCancel(decode(payload)?),
        "subscription.refresh" => Command::SubscriptionRefresh(decode(payload)?),
        "subscription.pause" => Command::SubscriptionPause(decode(payload)?),
        "subscription.resume" => Command::SubscriptionResume(decode(payload)?),
        "subscription.renew" => Command::SubscriptionRenew(decode(payload)?),
        "subscription.auto_renew" => Command::SubscriptionAutoRenew(decode(payload)?),
        "redemption.create" => Command::RedemptionCreate(decode(payload)?),
        "redemption.revoke" => Command::RedemptionRevoke(decode(payload)?),
        "api_key.write" => Command::Key(decode(payload)?),
        "api_key.rotate" => Command::KeyRotate(decode(payload)?),
        "api_key.delete" => Command::KeyDelete(decode(payload)?),
        "user.roles" => Command::Roles(decode(payload)?),
        "model.route" => Command::Route(decode(payload)?),
        "model.deployment" => Command::Deployment(decode(payload)?),
        "project.commercial" => Command::Commercial(decode(payload)?),
        _ => return Err("GOVERNANCE_UNSUPPORTED".into()),
    })
}
fn hash(value: &Value) -> Result<String, String> {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            _ => v.clone(),
        }
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(value)).map_err(|_| "invalid command")?)
    ))
}
fn numeric(id: &async_graphql::ID) -> Result<i64, String> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(id.as_str()) {
        Ok(guid.id)
    } else {
        id.as_str()
            .parse::<i64>()
            .map_err(|_| "invalid ID".to_string())
    }
}
fn effective_targets(command: &Command, mut targets: Vec<String>) -> Result<Vec<String>, String> {
    if let Command::ChannelOrderingForm(v) = command {
        let mut embedded =
            v.0.channels
                .iter()
                .map(|v| numeric(&v.id).map(|v| v.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
        embedded.sort();
        embedded.dedup();
        if !targets.is_empty() && targets != embedded {
            return Err("TARGET_MISMATCH".into());
        }
        targets = embedded;
    }
    let embedded = match command {
        Command::ChannelEndpointsForm(v) => Some(numeric(&v.0.channel_id)?.to_string()),
        Command::AliasesForm(v) => Some(numeric(&v.0.channel_id)?.to_string()),
        Command::RouteForm(v) => {
            v.0.id
                .as_ref()
                .map(numeric)
                .transpose()?
                .map(|v| v.to_string())
        }
        Command::MemberAddForm(v) => Some(numeric(&v.0.project_id)?.to_string()),
        Command::MemberUpdateForm(v) => Some(numeric(&v.0.project_id)?.to_string()),
        Command::MemberRemoveForm(v) => Some(numeric(&v.0.project_id)?.to_string()),
        Command::GroupUpdate(v) => Some(v.group_id.to_string()),
        Command::GroupAssign(v) => Some(v.group_id.to_string()),
        Command::GroupModels(v) => Some(v.group_id.to_string()),
        Command::GroupPrice(v) => Some(v.group_id.to_string()),
        Command::PlanUpdate(v) => Some(numeric(&v.id)?.to_string()),
        Command::Roles(v) => Some(v.user_id.to_string()),
        Command::Credit(v) => Some(numeric(&v.project_id)?.to_string()),
        Command::Member(v) | Command::MemberRemove(v) => {
            Some(v.project_id.ok_or("project_id is required")?.to_string())
        }
        Command::SpendLimit(v) => Some(v.project_id.to_string()),
        _ => None,
    };
    if let Some(id) = embedded {
        let id = id.to_string();
        if !targets.is_empty() && targets != [id.clone()] {
            return Err("TARGET_MISMATCH: targets must match the command object".into());
        }
        targets = vec![id];
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}
async fn dependency_rows(
    tx: &mut Transaction<'_, Postgres>,
    command: &Command,
    targets: &[String],
) -> Result<Vec<Value>, String> {
    let mut refs = std::collections::BTreeSet::<(&'static str, i64)>::new();
    let mut add = |table, id| {
        refs.insert((table, id));
    };
    match command {
        Command::RouteForm(v) => {
            add("models", numeric(&v.0.public_model_id)?);
            add("upstream_model_deployments", numeric(&v.0.deployment_id)?);
        }
        Command::PublicModelForm(v) => {
            for id in &v.0.deployment_ids {
                add("upstream_model_deployments", numeric(id)?);
            }
        }
        Command::SettingForm(v) => {
            if let Some(id) = v.storage_id().filter(|id| *id > 0) {
                add("data_storages", id);
            }
        }
        Command::MemberAddForm(v) => {
            add("users", numeric(&v.0.user_id)?);
            add("projects", numeric(&v.0.project_id)?);
            for id in v.0.role_ids.iter().flatten() {
                add("roles", numeric(id)?);
            }
        }
        Command::MemberUpdateForm(v) => {
            add("users", numeric(&v.0.user_id)?);
            add("projects", numeric(&v.0.project_id)?);
            for id in
                v.0.add_role_ids
                    .iter()
                    .flatten()
                    .chain(v.0.remove_role_ids.iter().flatten())
            {
                add("roles", numeric(id)?);
            }
        }
        Command::MemberRemoveForm(v) => {
            add("users", numeric(&v.0.user_id)?);
            add("projects", numeric(&v.0.project_id)?);
        }
        Command::UserCreateForm(v) => {
            for id in v.0.role_ids.iter().flatten() {
                add("roles", numeric(id)?);
            }
            for id in v.0.project_ids.iter().flatten() {
                add("projects", numeric(id)?);
            }
        }
        Command::UserUpdateForm(v) => {
            for id in
                v.0.add_role_ids
                    .iter()
                    .flatten()
                    .chain(v.0.remove_role_ids.iter().flatten())
            {
                add("roles", numeric(id)?);
            }
            for id in
                v.0.add_project_ids
                    .iter()
                    .flatten()
                    .chain(v.0.remove_project_ids.iter().flatten())
            {
                add("projects", numeric(id)?);
            }
        }
        Command::RoleCreateForm(v) => {
            if let Some(id) = &v.0.project_id {
                add("projects", numeric(id)?);
            }
        }
        Command::TemplateApply(v) => add("channel_override_templates", v.template_id),
        Command::KeyTemplateLoad(v) => add("api_key_profile_templates", v.template_id),
        Command::KeyTemplate(v) => {
            if let Some(p) = v.project_id {
                add("projects", p);
            }
        }
        Command::PublicModelRoutes(v) => {
            for id in &v.deployment_ids {
                add("upstream_model_deployments", *id);
            }
        }
        Command::Grant(v) => {
            if let Some(p) = v.project_id {
                add("projects", p);
            }
            if let Some(id) = v.access_plan_version_id {
                add("access_plan_versions", id);
            }
        }
        Command::Entitlement(v) => {
            if let Some(p) = v.project_id {
                add("projects", p);
            }
            if let Some(id) = v.public_model_id {
                add("models", id);
            }
        }
        Command::Adjustment(v) => {
            if let Some(p) = v.project_id {
                add("projects", p);
            }
        }
        Command::Member(v) | Command::MemberRemove(v) => {
            add("projects", v.project_id.ok_or("project_id is required")?);
            add("users", v.user_id.ok_or("user_id is required")?);
        }
        Command::SpendLimit(v) => {
            add("projects", v.project_id);
            add("users", v.user_id);
        }
        Command::Credit(v) => add("projects", numeric(&v.project_id)?),
        Command::Key(v) => {
            if let Some(id) = v.project_id {
                add("projects", id);
            }
            if let Some(id) = v.user_id {
                add("users", id);
            }
        }
        Command::Roles(v) => {
            add("users", v.user_id);
            for id in &v.role_ids {
                add("roles", *id);
            }
        }
        Command::Route(v) => {
            add("models", v.public_model_id);
            add("upstream_model_deployments", v.deployment_id);
        }
        Command::Deployment(v) => add("channels", v.channel_id),
        Command::Commercial(v) => {
            if let Some(id) = v.owner_user_id {
                add("users", id);
            }
            if let Some(id) = v.base_access_plan_id {
                add("access_plans", id);
            }
            if let Some(id) = v.base_price_tier_id {
                add("price_tiers", id);
            }
        }
        Command::SubscriptionAssign(v) => {
            add("users", numeric(&v.user_id)?);
            add("projects", numeric(&v.project_id)?);
            add("subscription_plans", numeric(&v.plan_id)?);
        }
        Command::PlanCreate(v) => {
            for id in &v.access_plan_ids {
                add("access_plans", numeric(id)?);
            }
        }
        Command::PlanUpdate(v) => {
            for id in &v.access_plan_ids {
                add("access_plans", numeric(id)?);
            }
        }
        Command::GroupCreate(v) => {
            for id in v.model_ids.iter().flatten() {
                add("models", numeric(id)?);
            }
            for id in v.route_ids.iter().flatten() {
                add("model_routes", numeric(id)?);
            }
            for id in v.user_ids.iter().flatten() {
                add("users", numeric(id)?);
            }
            if let Some(id) = &v.access_plan_id {
                add("access_plans", numeric(id)?);
            }
            if let Some(id) = &v.price_tier_id {
                add("price_tiers", numeric(id)?);
            }
            if let Some(id) = &v.default_subscription_plan_id {
                add("subscription_plans", numeric(id)?);
            }
        }
        Command::GroupUpdate(v) => {
            for id in v.model_ids.iter().flatten() {
                add("models", numeric(id)?);
            }
            for id in v.route_ids.iter().flatten() {
                add("model_routes", numeric(id)?);
            }
            for id in v.user_ids.iter().flatten() {
                add("users", numeric(id)?);
            }
            if let Some(id) = &v.default_subscription_plan_id {
                add("subscription_plans", numeric(id)?);
            }
        }
        Command::GroupAssign(v) => {
            for id in &v.user_ids {
                add("users", numeric(id)?);
            }
        }
        Command::GroupModels(v) => {
            for id in &v.model_ids {
                add("models", numeric(id)?);
            }
        }
        _ => {}
    }
    if matches!(
        command,
        Command::GroupUpdate(_)
            | Command::GroupPrice(_)
            | Command::GroupModels(_)
            | Command::GroupAssign(_)
            | Command::GroupDelete(_)
    ) {
        for target in targets {
            let (plan, tier): (i64, i64) = sqlx::query_as(
                "SELECT access_plan_id,price_tier_id FROM simple_groups WHERE id::text=$1 FOR UPDATE"
            )
            .bind(target)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| "group dependency lookup failed")?
            .ok_or("SOURCE_CHANGED: group dependency is missing")?;
            add("access_plans", plan);
            add("price_tiers", tier);
        }
    }
    let mut rows = Vec::new();
    for (table, id) in refs {
        let row: Option<Value> = sqlx::query_scalar(&format!(
            "SELECT to_jsonb(t) FROM {table} t WHERE id=$1 FOR UPDATE"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "dependency lookup failed")?;
        let row = row.ok_or("SOURCE_CHANGED: command dependency is missing")?;
        if row
            .get("deleted_at")
            .and_then(Value::as_i64)
            .is_some_and(|v| v != 0)
        {
            return Err("SOURCE_CHANGED: dependency is deleted".into());
        }
        let mut row = row;
        if table == "channels" {
            if let Some(m) = row.as_object_mut() {
                for field in ["quota", "quota_checked_at", "last_used_at", "updated_at"] {
                    m.remove(field);
                }
            }
        }
        rows.push(json!({"table":table,"id":id,"state":row}));
        if table == "access_plans" {
            let versions: Vec<Value> = sqlx::query_scalar(
                "SELECT to_jsonb(v) FROM access_plan_versions v WHERE access_plan_id=$1 AND status='published' ORDER BY id FOR UPDATE"
            ).bind(id).fetch_all(&mut **tx).await.map_err(|_| "access version dependency lookup failed")?;
            let ids = versions
                .iter()
                .filter_map(|v| v["id"].as_i64())
                .collect::<Vec<_>>();
            rows.push(json!({"plan":id,"publishedVersions":versions}));
            for query in [
                "SELECT to_jsonb(i) FROM access_plan_items i WHERE access_plan_version_id=ANY($1) ORDER BY access_plan_version_id,public_model_id FOR UPDATE",
                "SELECT to_jsonb(i) FROM access_plan_route_items i WHERE access_plan_version_id=ANY($1) ORDER BY access_plan_version_id,model_route_id FOR UPDATE",
            ] {
                let items: Vec<Value> = sqlx::query_scalar(query)
                    .bind(&ids)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(|_| "access scope dependency lookup failed")?;
                rows.push(json!({"plan":id,"relation":query,"state":items}));
            }
        }
    }
    // These relation sets are configuration too; hashing them detects edits
    // that do not change the parent object's timestamp.
    for target in targets {
        let queries: Vec<&str> = match command {
            Command::MemberAddForm(_)
            | Command::MemberUpdateForm(_)
            | Command::MemberRemoveForm(_)
            | Command::Member(_)
            | Command::MemberRemove(_)
            | Command::SpendLimit(_)
            | Command::Credit(_)
            | Command::Project(_)
            | Command::Commercial(_) => vec![
                "SELECT to_jsonb(t) FROM user_projects t WHERE project_id::text=$1 ORDER BY user_id",
                "SELECT to_jsonb(t) FROM project_commercial_profiles t WHERE project_id::text=$1",
            ],
            Command::Roles(_)
            | Command::User(_)
            | Command::UserUpdateForm(_)
            | Command::UserDelete(_) => vec![
                "SELECT to_jsonb(t) FROM user_roles t WHERE user_id::text=$1 ORDER BY role_id",
                "SELECT to_jsonb(t) FROM user_projects t WHERE user_id::text=$1 ORDER BY project_id",
            ],
            Command::GroupUpdate(_)
            | Command::GroupAssign(_)
            | Command::GroupModels(_)
            | Command::GroupPrice(_)
            | Command::GroupDelete(_) => vec![
                "SELECT to_jsonb(t) FROM simple_group_projects t WHERE simple_group_id::text=$1 ORDER BY project_id",
            ],
            Command::ChannelDuplicate(v) if v.include_prices => vec![
                "SELECT to_jsonb(t) FROM channel_model_prices t WHERE channel_id::text=$1 AND deleted_at=0 ORDER BY id",
            ],
            Command::AliasesForm(_) => vec![
                "SELECT jsonb_build_object('route',to_jsonb(r),'deployment',to_jsonb(d),'model',to_jsonb(m)) FROM model_routes r JOIN upstream_model_deployments d ON d.id=r.deployment_id JOIN models m ON m.id=r.public_model_id WHERE d.channel_id::text=$1 ORDER BY r.id",
            ],
            Command::PlanUpdate(_) => vec![
                "SELECT to_jsonb(t) FROM subscription_quota_rules t WHERE subscription_plan_id::text=$1 ORDER BY id",
                "SELECT to_jsonb(t) FROM subscription_plan_access_plans t WHERE subscription_plan_id::text=$1 ORDER BY access_plan_id",
            ],
            _ => vec![],
        };
        for query in queries {
            let values: Vec<Value> = sqlx::query_scalar(query)
                .bind(target)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "dependency relation lookup failed")?;
            rows.push(json!({"target":target,"relation":query,"state":values}));
        }
    }
    Ok(rows)
}
async fn command_projects(
    tx: &mut Transaction<'_, Postgres>,
    command: &Command,
    targets: &[String],
) -> Result<Vec<i64>, String> {
    let projects: Vec<i64> = match command {
        Command::KeyCreateForm(v) => vec![numeric(&v.0.project_id)?],
        Command::KeyUpdateForm(_) | Command::KeyProfilesForm(_) => {
            sqlx::query_scalar("SELECT DISTINCT project_id FROM api_keys WHERE id::text=ANY($1)")
                .bind(targets)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "key scope lookup failed")?
        }
        Command::MemberAddForm(v) => vec![numeric(&v.0.project_id)?],
        Command::MemberUpdateForm(v) => vec![numeric(&v.0.project_id)?],
        Command::MemberRemoveForm(v) => vec![numeric(&v.0.project_id)?],
        Command::RoleCreateForm(v) => {
            v.0.project_id
                .as_ref()
                .map(numeric)
                .transpose()?
                .into_iter()
                .collect()
        }
        Command::Member(v) | Command::MemberRemove(v) => {
            vec![v.project_id.ok_or("project_id is required")?]
        }
        Command::SpendLimit(v) => vec![v.project_id],
        Command::Credit(v) => vec![numeric(&v.project_id)?],
        Command::Project(_)
        | Command::ProjectProfilesForm(_)
        | Command::ProjectUpdateForm(_)
        | Command::ProjectDelete(_)
        | Command::Commercial(_) => targets
            .iter()
            .map(|id| id.parse().map_err(|_| "invalid Project target"))
            .collect::<Result<_, _>>()?,
        Command::Key(v) => {
            if let Some(p) = v.project_id {
                vec![p]
            } else {
                sqlx::query_scalar(
                    "SELECT DISTINCT project_id FROM api_keys WHERE id::text=ANY($1)",
                )
                .bind(targets)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "key scope lookup failed")?
            }
        }
        Command::KeyRotate(_) | Command::KeyDelete(_) => {
            sqlx::query_scalar("SELECT DISTINCT project_id FROM api_keys WHERE id::text=ANY($1)")
                .bind(targets)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "key scope lookup failed")?
        }
        Command::Role(v) if v.level.as_deref() == Some("project") => {
            vec![v.project_id.ok_or("project role requires project_id")?]
        }
        Command::SubscriptionAssign(v) => vec![numeric(&v.project_id)?],
        Command::KeyTemplateCreateForm(v) => vec![numeric(&v.input.0.project_id)?],
        Command::KeyTemplateUpdateForm(_) => sqlx::query_scalar(
            "SELECT DISTINCT project_id FROM api_key_profile_templates WHERE id::text=ANY($1)",
        )
        .bind(targets)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| "template scope lookup failed")?,
        Command::KeyTemplate(v) => {
            if let Some(p) = v.project_id {
                vec![p]
            } else {
                sqlx::query_scalar("SELECT DISTINCT project_id FROM api_key_profile_templates WHERE id::text=ANY($1)").bind(targets).fetch_all(&mut **tx).await.map_err(|_|"template scope lookup failed")?
            }
        }
        Command::KeyTemplateDelete(_) => sqlx::query_scalar(
            "SELECT DISTINCT project_id FROM api_key_profile_templates WHERE id::text=ANY($1)",
        )
        .bind(targets)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| "template scope lookup failed")?,
        Command::KeyTemplateLoad(_) => {
            sqlx::query_scalar("SELECT DISTINCT project_id FROM api_keys WHERE id::text=ANY($1)")
                .bind(targets)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "key scope lookup failed")?
        }
        Command::Grant(v) => v.project_id.into_iter().collect(),
        Command::Entitlement(v) => v.project_id.into_iter().collect(),
        Command::Adjustment(v) => v.project_id.into_iter().collect(),
        Command::Role(_) | Command::RoleUpdateForm(_) | Command::RoleDelete(_) => {
            sqlx::query_scalar(
                "SELECT DISTINCT project_id FROM roles WHERE id::text=ANY($1) AND level='project'",
            )
            .bind(targets)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| "role scope lookup failed")?
        }
        _ => vec![],
    };
    Ok(projects)
}
fn group_writes_price(command: &Command) -> bool {
    matches!(command, Command::GroupPrice(_))
        || matches!(command, Command::GroupCreate(v) if v.multiplier_ppm.is_some())
        || matches!(command, Command::GroupUpdate(v) if v.multiplier_ppm.is_some())
}
async fn authorize_command(
    tx: &mut Transaction<'_, Postgres>,
    actor: i64,
    action: &str,
    command: &Command,
    targets: &[String],
) -> Result<(), String> {
    let scope = registration(action)?.1;
    if matches!(command,Command::Setting(v) if matches!(v.key.as_str(),"system_product_experience_settings"|"system_auto_backup_settings"))
        || matches!(
            command,
            Command::SettingForm(
                crate::wiring_governance_settings::SettingForm::ProductExperience(_)
                    | crate::wiring_governance_settings::SettingForm::Backup(_)
            )
        )
    {
        let owner:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND is_owner AND status='activated' AND deleted_at=0)").bind(actor).fetch_one(&mut **tx).await.map_err(|_|"owner authority unavailable")?;
        if !owner {
            return Err("FORBIDDEN: this setting requires current Owner authority".into());
        }
    }

    let projects = command_projects(tx, command, targets).await?;
    if matches!(command,Command::Channel(v) if v.settings.is_some())
        || matches!(command,Command::ChannelCreateForm(v) if v.0.settings.is_some())
        || matches!(command,Command::ChannelUpdateForm(v) if v.0.settings.is_some())
        || matches!(command,Command::ChannelBatch(v) if v.channels.iter().any(|v|v.settings.is_some()))
        || matches!(command,Command::ChannelBulkCreateForm(v) if v.0.settings.is_some())
    {
        policy::authorize(tx, actor, "write_commercialization").await?;
    }
    if matches!(command,Command::ChannelDuplicate(v) if v.include_prices)
        || matches!(command, Command::ChannelDuplicateForm(_))
    {
        policy::authorize(tx, actor, "write_commercialization").await?;
    }
    if group_writes_price(command) || matches!(command, Command::Supply(_)) {
        let p = policy::lock_policy(tx).await?;
        if (group_writes_price(command) && p.requires("retail_price"))
            || (matches!(command, Command::Supply(_)) && p.requires("model_mapping"))
        {
            policy::authorize(tx, actor, "write_commercialization").await?;
        }
    }
    if projects.is_empty() {
        policy::authorize(tx, actor, scope).await?;
    } else {
        for project in &projects {
            policy::authorize_project(tx, actor, scope, *project).await?;
        }
    }
    if matches!(command, Command::Roles(_))
        || matches!(command,Command::User(v) if v.scopes.is_some())
    {
        policy::authorize(tx, actor, "write_roles").await?;
    }
    let mut grants = match command {
        Command::KeyCreateForm(v) => {
            if v.0.key_type == Some(conduit_admin_graphql::apikey::APIKeyType::ServiceAccount) {
                v.0.scopes.clone().unwrap_or_default()
            } else {
                vec![]
            }
        }
        Command::KeyUpdateForm(v) => {
            v.0.scopes
                .iter()
                .flatten()
                .chain(v.0.append_scopes.iter().flatten())
                .cloned()
                .collect()
        }
        Command::MemberAddForm(v) => v.0.scopes.clone().unwrap_or_default(),
        Command::MemberUpdateForm(v) => v.0.scopes.clone().unwrap_or_default(),
        Command::UserCreateForm(v) => v.0.scopes.clone().unwrap_or_default(),
        Command::UserUpdateForm(v) => {
            v.0.scopes
                .iter()
                .flatten()
                .chain(v.0.append_scopes.iter().flatten())
                .cloned()
                .collect()
        }
        Command::RoleCreateForm(v) => v.0.scopes.clone().unwrap_or_default(),
        Command::RoleUpdateForm(v) => v.0.scopes.clone().unwrap_or_default(),
        Command::User(v) => v.scopes.clone().unwrap_or_default(),
        Command::Role(v) => v.scopes.clone().unwrap_or_default(),
        Command::Member(v) => v.scopes.clone().unwrap_or_default(),
        Command::Key(v) => v.scopes.clone().unwrap_or_default(),
        _ => vec![],
    };
    if let Command::Roles(v) = command {
        let role_scopes: Vec<Value> =
            sqlx::query_scalar("SELECT scopes FROM roles WHERE id=ANY($1) AND deleted_at=0")
                .bind(&v.role_ids)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| "role grant authority lookup failed")?;
        for value in role_scopes {
            grants.extend(decode::<Vec<String>>(value)?);
        }
    }
    let role_ids = match command {
        Command::MemberAddForm(v) => v.0.role_ids.clone().unwrap_or_default(),
        Command::MemberUpdateForm(v) => v.0.add_role_ids.clone().unwrap_or_default(),
        Command::UserCreateForm(v) => v.0.role_ids.clone().unwrap_or_default(),
        Command::UserUpdateForm(v) => v.0.add_role_ids.clone().unwrap_or_default(),
        _ => vec![],
    };
    if !role_ids.is_empty()
        || matches!(command,Command::MemberUpdateForm(v) if v.0.remove_role_ids.is_some())
        || matches!(command,Command::UserCreateForm(v) if v.0.scopes.is_some())
        || matches!(command,Command::UserUpdateForm(v) if v.0.scopes.is_some()||v.0.append_scopes.is_some()||v.0.clear_scopes.is_some()||v.0.clear_roles.is_some()||v.0.remove_role_ids.is_some())
    {
        if projects.is_empty() {
            policy::authorize(tx, actor, "write_roles").await?;
        } else {
            for p in &projects {
                policy::authorize_project(tx, actor, "write_roles", *p).await?;
            }
        }
    }
    for role in role_ids {
        let scopes: Value =
            sqlx::query_scalar("SELECT scopes FROM roles WHERE id=$1 AND deleted_at=0")
                .bind(numeric(&role)?)
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| "role grant source is missing")?;
        grants.extend(decode::<Vec<String>>(scopes)?);
    }
    if matches!(command,Command::UserCreateForm(v) if v.0.is_owner==Some(true))
        || matches!(command,Command::UserUpdateForm(v) if v.0.is_owner.is_some())
    {
        let owner: bool = sqlx::query_scalar(
            "SELECT is_owner FROM users WHERE id=$1 AND deleted_at=0 AND status='activated'",
        )
        .bind(actor)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| "owner authority lookup failed")?;
        if !owner {
            return Err("FORBIDDEN: only Owner can change system ownership".into());
        }
    }
    if matches!(command,Command::MemberAddForm(v) if v.0.is_owner==Some(true))
        || matches!(command,Command::MemberUpdateForm(v) if v.0.is_owner==Some(true))
        || matches!(command,Command::Member(v) if v.is_owner==Some(true))
    {
        for project in &projects {
            let owner:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND is_owner AND deleted_at=0 AND status='activated') OR EXISTS(SELECT 1 FROM user_projects up JOIN users u ON u.id=up.user_id WHERE up.user_id=$1 AND up.project_id=$2 AND up.is_owner AND u.deleted_at=0 AND u.status='activated')").bind(actor).bind(project).fetch_one(&mut **tx).await.map_err(|_|"Project ownership authority unavailable")?;
            if !owner {
                return Err("FORBIDDEN: only a current Owner can grant Project ownership".into());
            }
        }
    }
    for granted in grants {
        if granted == "*" {
            let owner: bool = sqlx::query_scalar(
                "SELECT is_owner FROM users WHERE id=$1 AND status='activated' AND deleted_at=0",
            )
            .bind(actor)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| "grant authority lookup failed")?;
            if !owner {
                return Err("FORBIDDEN: only Owner can grant wildcard authority".into());
            }
        } else if projects.is_empty() {
            policy::authorize(tx, actor, &granted).await?;
        } else {
            for project in &projects {
                policy::authorize_project(tx, actor, &granted, *project).await?;
            }
        }
    }
    Ok(())
}

async fn validate_ownership(
    tx: &mut Transaction<'_, Postgres>,
    maker: i64,
    command: &Command,
    targets: &[String],
) -> Result<(), String> {
    let mut ids = if matches!(
        command,
        Command::OverrideTemplate(_)
            | Command::ChannelTemplateUpdateForm(_)
            | Command::OverrideTemplateDelete(_)
    ) {
        targets
            .iter()
            .map(|s| s.parse::<i64>().map_err(|_| "invalid template ID"))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![]
    };
    if let Command::TemplateApply(v) = command {
        ids.push(v.template_id);
    }
    let other: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM channel_override_templates WHERE id=ANY($1) AND user_id<>$2)",
    )
    .bind(ids)
    .bind(maker)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| "template ownership lookup failed")?;
    if other {
        return Err("FORBIDDEN: template belongs to another user".into());
    }
    Ok(())
}
fn table(command: &Command) -> Option<&'static str> {
    match command {
        Command::ChannelDuplicateForm(_)
        | Command::ChannelEndpointsForm(_)
        | Command::ChannelOrderingForm(_)
        | Command::AliasesForm(_)
        | Command::ChannelUpdateForm(_)
        | Command::ChannelDuplicate(_)
        | Command::ChannelKeys(_)
        | Command::TemplateApply(_)
        | Command::TemplateClear(_) => Some("channels"),
        Command::OverrideTemplate(_)
        | Command::ChannelTemplateUpdateForm(_)
        | Command::OverrideTemplateDelete(_) => Some("channel_override_templates"),
        Command::KeyTemplate(_)
        | Command::KeyTemplateUpdateForm(_)
        | Command::KeyTemplateDelete(_) => Some("api_key_profile_templates"),
        Command::KeyTemplateLoad(_) => Some("api_keys"),
        Command::Protection(_)
        | Command::ProtectionUpdateForm(_)
        | Command::ProtectionDelete(_) => Some("prompt_protection_rules"),
        Command::Storage(_) | Command::StorageUpdateForm(_) => Some("data_storages"),
        Command::AccessPlan(_) => Some("access_plans"),
        Command::PriceTier(_) => Some("price_tiers"),
        Command::Grant(_) => Some("project_access_grants"),
        Command::Entitlement(_) => Some("project_entitlement_overrides"),
        Command::Adjustment(_) => Some("project_price_adjustments"),
        Command::Channel(_) | Command::ChannelDelete(_) | Command::Supply(_) => Some("channels"),
        Command::Model(_) | Command::ModelUpdateForm(_) | Command::ModelDelete(_) => Some("models"),
        Command::Project(_)
        | Command::ProjectProfilesForm(_)
        | Command::ProjectUpdateForm(_)
        | Command::ProjectDelete(_) => Some("projects"),
        Command::User(_) | Command::UserUpdateForm(_) | Command::UserDelete(_) => Some("users"),
        Command::Role(_) | Command::RoleUpdateForm(_) | Command::RoleDelete(_) => Some("roles"),
        Command::GroupUpdate(_)
        | Command::GroupAssign(_)
        | Command::GroupModels(_)
        | Command::GroupPrice(_)
        | Command::GroupDelete(_) => Some("simple_groups"),
        Command::Key(_)
        | Command::KeyUpdateForm(_)
        | Command::KeyProfilesForm(_)
        | Command::KeyRotate(_)
        | Command::KeyDelete(_) => Some("api_keys"),
        Command::Roles(_) => Some("users"),
        Command::Route(_) | Command::RouteForm(_) => Some("model_routes"),
        Command::Deployment(_) => Some("upstream_model_deployments"),
        Command::Commercial(_) => Some("projects"),
        Command::PlanUpdate(_) => Some("subscription_plans"),
        Command::SubscriptionCancel(_)
        | Command::SubscriptionRefresh(_)
        | Command::SubscriptionPause(_)
        | Command::SubscriptionResume(_)
        | Command::SubscriptionRenew(_)
        | Command::SubscriptionAutoRenew(_) => Some("user_subscriptions"),
        Command::RedemptionRevoke(_) => Some("credit_redemption_codes"),
        _ => None,
    }
}
async fn target_revision(
    tx: &mut Transaction<'_, Postgres>,
    command: &Command,
    targets: &[String],
) -> Result<String, String> {
    let mut rows = dependency_rows(tx, command, targets).await?;
    if let Some(table) = table(command) {
        for id in targets {
            let row: Option<Value> = sqlx::query_scalar(&format!(
                "SELECT to_jsonb(t) FROM {table} t WHERE id::text=$1 FOR UPDATE"
            ))
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| "target lookup failed")?;
            let row = row.ok_or("SOURCE_CHANGED: target is missing")?;
            if row
                .get("deleted_at")
                .and_then(Value::as_i64)
                .is_some_and(|v| v != 0)
            {
                return Err("SOURCE_CHANGED: target is deleted".into());
            }
            let row = Some(row).map(|mut v| {
                if table == "channels" {
                    if let Some(m) = v.as_object_mut() {
                        for key in ["quota", "quota_checked_at", "last_used_at", "updated_at"] {
                            m.remove(key);
                        }
                    }
                }
                v
            });
            rows.push(json!({"id":id,"state":row}));
        }
    } else if let Command::Setting(s) = command {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(&s.key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "target lookup failed")?;
        rows.push(json!({"key":s.key,"state":raw}));
    } else if let Command::SettingForm(s) = command {
        for key in s.keys() {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
            )
            .bind(key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| "target lookup failed")?;
            let mut value = raw.map(|v| serde_json::from_str::<Value>(&v).unwrap_or(json!(v)));
            if key == conduit_services::system_service::system_key::AUTO_BACKUP_SETTINGS {
                if let Some(m) = value.as_mut().and_then(Value::as_object_mut) {
                    m.remove("last_backup_at");
                    m.remove("last_backup_error");
                }
            }
            rows.push(json!({"key":key,"state":value}));
        }
    } else if let Command::SpendLimit(s) = command {
        let limit:Option<Option<i64>>=sqlx::query_scalar("SELECT limit_micros FROM project_member_spend_state WHERE project_id=$1 AND user_id=$2 FOR UPDATE").bind(s.project_id).bind(s.user_id).fetch_optional(&mut **tx).await.map_err(|_|"target lookup failed")?;
        rows.push(json!({"projectID":s.project_id,"userID":s.user_id,"limit":limit}));
    } else if let Command::MappingAutomation(_) = command {
        let raw:Option<String>=sqlx::query_scalar("SELECT value FROM systems WHERE key='channel_model_mapping_automation_enabled' AND deleted_at=0 FOR UPDATE").fetch_optional(&mut **tx).await.map_err(|_|"mapping policy lookup failed")?;
        rows.push(json!({"state":raw}));
    }
    hash(&json!(rows))
}
fn normalize_targets(targets: Vec<async_graphql::ID>) -> Result<Vec<String>, String> {
    let mut result = targets
        .into_iter()
        .map(|id| {
            if let Ok(guid) = conduit_admin_graphql::node::parse_guid(id.as_str()) {
                Ok(guid.id.to_string())
            } else if id.as_str().parse::<i64>().is_ok()
                || uuid::Uuid::parse_str(id.as_str()).is_ok()
            {
                Ok(id.to_string())
            } else {
                Err("invalid target ID".into())
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    result.sort();
    result.dedup();
    Ok(result)
}
pub(crate) async fn base(
    pool: &PgPool,
    actor: i64,
    action: &str,
    targets: Vec<async_graphql::ID>,
    payload: Value,
) -> Result<String, String> {
    registration(action)?;
    let command = command(action, payload)?;
    let targets = effective_targets(&command, normalize_targets(targets)?)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "governance transaction unavailable")?;
    policy::lock_policy(&mut tx).await?;
    authorize_command(&mut tx, actor, action, &command, &targets).await?;
    validate_ownership(&mut tx, actor, &command, &targets).await?;
    validate_command(&command, &targets)?;
    let revision = target_revision(&mut tx, &command, &targets).await?;
    tx.commit().await.map_err(|_| "target lookup failed")?;
    Ok(revision)
}
pub(crate) async fn propose(
    pool: &PgPool,
    actor: i64,
    input: gql::ProposeGovernedChangeInput,
) -> Result<package::ChangeSetDetail, String> {
    if input.reason.trim().is_empty() {
        return Err("reason is required".into());
    }
    let (kind, _) = registration(&input.action)?;
    let original_payload = input.payload.0;
    let mut payload = original_payload.clone();
    let mut command = command(&input.action, payload.clone())?;
    if let Command::User(user) = &mut command {
        if let Some(password) = &user.password {
            user.password = Some(
                conduit_auth::encode_password_bcrypt_hex(
                    password,
                    conduit_auth::DEFAULT_BCRYPT_COST,
                )
                .map_err(|_| "password preparation failed")?,
            );
        }
        payload = serde_json::to_value(user).map_err(|_| "invalid command")?;
    }
    match &mut command {
        Command::UserCreateForm(v) => {
            conduit_admin_graphql::user::validate_create_user_input(&v.0)
                .map_err(|_| "invalid user form")?;
            v.0.password = conduit_auth::encode_password_bcrypt_hex(
                &v.0.password,
                conduit_auth::DEFAULT_BCRYPT_COST,
            )
            .map_err(|_| "password preparation failed")?;
            payload = serde_json::to_value(v).map_err(|_| "invalid user form")?;
        }
        Command::UserUpdateForm(v) => {
            conduit_admin_graphql::user::validate_update_user_input(&v.0)
                .map_err(|_| "invalid user form")?;
            if let Some(password) = &v.0.password {
                v.0.password = Some(
                    conduit_auth::encode_password_bcrypt_hex(
                        password,
                        conduit_auth::DEFAULT_BCRYPT_COST,
                    )
                    .map_err(|_| "password preparation failed")?,
                );
            }
            payload = serde_json::to_value(v).map_err(|_| "invalid user form")?;
        }
        _ => {}
    }
    let targets = effective_targets(&command, normalize_targets(input.target_ids)?)?;
    validate_command(&command, &targets)?;
    let operation = uuid::Uuid::parse_str(input.operation_id.as_str())
        .map_err(|_| "operationID must be UUID")?;
    // Bind the original request fingerprint, before randomized password hashing.
    let fingerprint = hash(
        &json!({"actor":actor,"action":input.action,"targets":targets,"payload":original_payload,"expected":input.expected_target_revision,"reason":input.reason.trim()}),
    )?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "governance transaction unavailable")?;
    let policy = policy::lock_policy(&mut tx).await?;
    authorize_command(&mut tx, actor, &input.action, &command, &targets).await?;
    if let Command::SettingForm(v) = &command {
        v.validate(&mut tx).await?;
    }
    validate_ownership(&mut tx, actor, &command, &targets).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(operation.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|_| "operation lock failed")?;
    if let Some((old_actor,old_action,old_fingerprint,id))=sqlx::query_as::<_,(i64,String,String,i64)>("SELECT actor_id,action,fingerprint,change_set_id FROM governed_change_operations WHERE operation_id=$1").bind(operation).fetch_optional(&mut *tx).await.map_err(|_|"operation lookup failed")? {
        if old_actor!=actor || old_action!=input.action || old_fingerprint!=fingerprint {return Err("OPERATION_CONFLICT".into());}
        tx.commit().await.map_err(|_|"operation replay failed")?;
        return crate::wiring_postgres_change_sets::packages::required_detail(pool,actor,id).await.map_err(|_|"package lookup failed".into());
    }
    let current = target_revision(&mut tx, &command, &targets).await?;
    if current != input.expected_target_revision {
        return Err(format!(
            "REVISION_CONFLICT: current target revision {current}"
        ));
    }
    let id:i64=sqlx::query_scalar("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) VALUES($1,$2,'governed','0',$3,'draft',$4,$5,$6,now(),now()) RETURNING id").bind(operation).bind(kind).bind(input.reason.trim()).bind(&current).bind(policy.revision.to_string()).bind(actor).fetch_one(&mut *tx).await.map_err(|_|"package creation failed")?;
    let private = json!({"payload":payload,"targets":targets});
    sqlx::query("INSERT INTO governed_change_commands(change_set_id,item_key,action,private_payload,target_revision,domain_scope,project_ids) VALUES($1,'command',$2,$3,$4,$5,$6)").bind(id).bind(&input.action).bind(sqlx::types::Json(private)).bind(&current).bind(registration(&input.action)?.1).bind(command_projects(&mut tx,&command,&targets).await?).execute(&mut *tx).await.map_err(|_|"command storage failed")?;
    sqlx::query("INSERT INTO change_set_items(change_set_id,item_key,action,after_snapshot,created_at,updated_at) VALUES($1,'command','update',$2,now(),now())").bind(id).bind(sqlx::types::Json(json!({"action":input.action,"targetIDs":targets}))).execute(&mut *tx).await.map_err(|_|"command summary failed")?;
    sqlx::query("INSERT INTO governed_change_operations(operation_id,actor_id,action,fingerprint,change_set_id) VALUES($1,$2,$3,$4,$5)").bind(operation).bind(actor).bind(&input.action).bind(fingerprint).bind(id).execute(&mut *tx).await.map_err(|_|"operation storage failed")?;
    crate::wiring_postgres_change_sets::insert_event(
        &mut tx,
        id,
        "created",
        "user",
        Some(actor),
        json!({"action":input.action,"targetIDs":targets}),
        chrono::Utc::now(),
    )
    .await
    .map_err(|_| "command audit failed")?;
    tx.commit().await.map_err(|_| "command commit failed")?;
    crate::wiring_postgres_change_sets::packages::required_detail(pool, actor, id)
        .await
        .map_err(|_| "package lookup failed".into())
}
pub(crate) async fn validate(
    pool: &PgPool,
    actor: i64,
    id: async_graphql::ID,
    expected: String,
) -> Result<package::ChangeSetDetail, String> {
    let id = id
        .as_str()
        .parse::<i64>()
        .map_err(|_| "invalid package ID")?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "governance transaction unavailable")?;
    policy::lock_policy(&mut tx).await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM change_sets WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| "package lock failed")?
            .ok_or("package missing")?;
    if !matches!(status.as_str(), "draft" | "pending_review" | "approved") {
        return Err("INVALID_TRANSITION: terminal package is immutable".into());
    }
    let current: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(id),0) FROM change_set_events WHERE change_set_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "revision lookup failed")?;
    if current.to_string() != expected {
        return Err("REVISION_CONFLICT".into());
    }
    check(&mut tx, id, Some(actor)).await?;
    crate::wiring_postgres_change_sets::insert_event(
        &mut tx,
        id,
        "validated",
        "user",
        Some(actor),
        json!({}),
        chrono::Utc::now(),
    )
    .await
    .map_err(|_| "validation audit failed")?;
    tx.commit().await.map_err(|_| "validation commit failed")?;
    crate::wiring_postgres_change_sets::packages::required_detail(pool, actor, id)
        .await
        .map_err(|_| "package lookup failed".into())
}
pub(crate) async fn check(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    actor: Option<i64>,
) -> Result<(), String> {
    let commands=sqlx::query("SELECT action,private_payload,target_revision FROM governed_change_commands WHERE change_set_id=$1 ORDER BY item_key").bind(id).fetch_all(&mut **tx).await.map_err(|_|"command lookup failed")?;
    if commands.is_empty() {
        return Err("EMPTY_PACKAGE".into());
    }
    for row in commands {
        let action: String = row.get("action");
        let data: Value = row.get("private_payload");
        let command = command(&action, data["payload"].clone())?;
        let targets: Vec<String> = decode(data["targets"].clone())?;
        if let Some(actor) = actor {
            authorize_command(tx, actor, &action, &command, &targets).await?;
        }
        let maker: Option<i64> =
            sqlx::query_scalar("SELECT created_by FROM change_sets WHERE id=$1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| "maker lookup failed")?;
        if let Some(maker) = maker {
            validate_ownership(tx, maker, &command, &targets).await?;
        }
        validate_command(&command, &targets)?;
        if let Command::SettingForm(v) = &command {
            v.validate(tx).await?;
        }
        if target_revision(tx, &command, &targets).await? != row.get::<String, _>("target_revision")
        {
            return Err("SOURCE_CHANGED".into());
        }
    }
    Ok(())
}
pub(crate) async fn stage_supply(
    tx: &mut Transaction<'_, Postgres>,
    context: &crate::wiring_postgres_change_sets::OperationContext,
    channel: i64,
    models: Vec<String>,
    observation: i64,
    p: &Policy,
) -> Result<Option<i64>, String> {
    let current: Value =
        sqlx::query_scalar("SELECT supported_models FROM channels WHERE id=$1 FOR UPDATE")
            .bind(channel)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| "supply lookup failed")?;
    if current == json!(models) {
        return Ok(None);
    }
    let targets = vec![channel.to_string()];
    let command = Command::Supply(SupplyWrite {
        supported_models: models.clone(),
    });
    let revision = target_revision(tx, &command, &targets).await?;
    if !p.requires("model_supply") && !p.requires("model_mapping") {
        apply_supply(tx, &models, &targets).await?;
        return Ok(None);
    }
    let superseded:Vec<i64>=sqlx::query_scalar("UPDATE change_sets SET status='superseded',reviewed_at=now(),updated_at=now() WHERE kind='model_supply' AND scope_type='governed' AND scope_id=$1 AND created_by IS NULL AND status IN ('pending_review','approved') RETURNING id").bind(channel.to_string()).fetch_all(&mut **tx).await.map_err(|_|"supply supersession failed")?;
    for old in superseded {
        crate::wiring_postgres_change_sets::insert_event(tx,old,"superseded","system",None,json!({"reason":"complete_observation_changed","replacementOperationID":context.operation_id}),chrono::Utc::now()).await.map_err(|_|"supply supersession audit failed")?;
    }
    let id:i64=sqlx::query_scalar("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,submitted_by,submitted_at,created_at,updated_at) VALUES($1,'model_supply','governed',$2,'Observed model supply','pending_review',$3,$4,$5,$5,now(),now(),now()) RETURNING id").bind(context.operation_id).bind(channel.to_string()).bind(&revision).bind(observation.to_string()).bind(context.actor_user_id).fetch_one(&mut **tx).await.map_err(|_|"supply proposal failed")?;
    sqlx::query("INSERT INTO governed_change_commands(change_set_id,item_key,action,private_payload,target_revision,domain_scope) VALUES($1,'command','model.supply',$2,$3,'write_channels')").bind(id).bind(sqlx::types::Json(json!({"payload":{"supported_models":models},"targets":targets}))).bind(&revision).execute(&mut **tx).await.map_err(|_|"supply proposal failed")?;
    sqlx::query("INSERT INTO change_set_items(change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) VALUES($1,'command','update',$2,$3,now(),now())").bind(id).bind(sqlx::types::Json(json!({"action":"model.supply","targetIDs":targets,"modelCount":models.len()}))).bind(sqlx::types::Json(json!({"source":"observed","channelID":channel,"observationID":observation}))).execute(&mut **tx).await.map_err(|_|"supply summary failed")?;
    crate::wiring_postgres_change_sets::insert_event(
        tx,
        id,
        "created",
        "system",
        context.actor_user_id,
        json!({"origin":format!("{:?}",context.origin),"observationID":observation}),
        chrono::Utc::now(),
    )
    .await
    .map_err(|_| "supply audit failed")?;
    Ok(Some(id))
}
async fn apply_supply(
    tx: &mut Transaction<'_, Postgres>,
    models: &[String],
    targets: &[String],
) -> Result<(), String> {
    for target in targets {
        sqlx::query("UPDATE channels SET supported_models=$1,updated_at=now() WHERE id::text=$2 AND deleted_at=0").bind(sqlx::types::Json(models)).bind(target).execute(&mut **tx).await.map_err(|_|"supply update failed")?;
        sqlx::query("UPDATE upstream_model_deployments SET status='disabled',updated_at=now() WHERE channel_id::text=$1 AND source='discovered' AND NOT(upstream_model_id=ANY($2))").bind(target).bind(models).execute(&mut **tx).await.map_err(|_|"supply update failed")?;
        sqlx::query("UPDATE model_routes r SET status='disabled',updated_at=now() FROM upstream_model_deployments d WHERE r.deployment_id=d.id AND d.channel_id::text=$1 AND d.status='disabled'").bind(target).execute(&mut **tx).await.map_err(|_|"supply update failed")?;
    }
    Ok(())
}
pub(crate) async fn authorize_package(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    actor: i64,
) -> Result<(), String> {
    let actions=sqlx::query("SELECT action,private_payload FROM governed_change_commands WHERE change_set_id=$1 ORDER BY item_key").bind(id).fetch_all(&mut **tx).await.map_err(|_|"command lookup failed")?;
    if actions.is_empty() {
        return Err("EMPTY_PACKAGE".into());
    }
    for row in actions {
        let action: String = row.get("action");
        let data: Value = row.get("private_payload");
        let c = command(&action, data["payload"].clone())?;
        let targets: Vec<String> = decode(data["targets"].clone())?;
        authorize_command(tx, actor, &action, &c, &targets).await?;
    }
    Ok(())
}
pub(crate) async fn requires_review(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    p: &Policy,
) -> Result<bool, String> {
    let rows = sqlx::query(
        "SELECT action,private_payload FROM governed_change_commands WHERE change_set_id=$1",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| "command policy lookup failed")?;
    for row in rows {
        let action: String = row.get("action");
        let data: Value = row.get("private_payload");
        let c = command(&action, data["payload"].clone())?;
        if p.requires(registration(&action)?.0) {
            return Ok(true);
        }
        let mapping = matches!(&c,Command::ChannelCreateForm(v) if v.0.settings.is_some())
            || matches!(&c,Command::ChannelUpdateForm(v) if v.0.settings.is_some())
            || matches!(&c,Command::Channel(v) if v.settings.is_some())
            || matches!(
                &c,
                Command::ChannelBatch(_)
                    | Command::ChannelBulkCreateForm(_)
                    | Command::ChannelDuplicate(_)
                    | Command::ChannelDuplicateForm(_)
            );
        let supply = match &c {
            Command::ChannelCreateForm(v) => {
                !v.0.supported_models.is_empty()
                    || v.0.manual_models.as_ref().is_some_and(|v| !v.is_empty())
            }
            Command::ChannelUpdateForm(v) => {
                v.0.supported_models.is_some() || v.0.manual_models.is_some()
            }
            Command::Channel(v) => v.supported_models.is_some() || v.manual_models.is_some(),
            Command::ChannelBatch(v) => v
                .channels
                .iter()
                .any(|c| c.supported_models.is_some() || c.manual_models.is_some()),
            Command::ChannelBulkCreateForm(v) => !v.0.supported_models.is_empty(),
            Command::ChannelImportForm(v) => {
                v.0.channels.iter().any(|v| !v.supported_models.is_empty())
            }
            Command::ChannelDuplicate(_) | Command::ChannelDuplicateForm(_) => true,
            _ => false,
        };
        let price = group_writes_price(&c);
        let access = matches!(
            &c,
            Command::GroupModels(_) | Command::GroupAssign(_) | Command::GroupDelete(_)
        ) || matches!(&c,Command::GroupCreate(v) if v.model_ids.is_some()||v.user_ids.is_some())
            || matches!(&c,Command::GroupUpdate(v) if v.model_ids.is_some()||v.route_ids.is_some()||v.user_ids.is_some());
        if (matches!(&c, Command::ProjectCreateForm(_))
            || matches!(&c,Command::Project(_) if data["targets"].as_array().is_some_and(|v|v.is_empty())))
            && p.requires("identity_access")
        {
            return Ok(true);
        }
        if mapping && p.requires("model_mapping")
            || supply && p.requires("model_supply")
            || price && p.requires("retail_price")
            || access && p.requires("project_configuration")
            || matches!(&c, Command::Supply(_)) && p.requires("model_mapping")
            || (matches!(&c,Command::ChannelDuplicate(v) if v.include_prices)
                || matches!(&c, Command::ChannelDuplicateForm(_)))
                && p.requires("provider_price")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn validate_command(command: &Command, targets: &[String]) -> Result<(), String> {
    let status = |value: &Option<String>, allowed: &[&str]| -> Result<(), String> {
        if value
            .as_ref()
            .is_some_and(|s| !allowed.contains(&s.as_str()))
        {
            Err("invalid domain status".into())
        } else {
            Ok(())
        }
    };
    match command {
        Command::PublicModelForm(_) | Command::PriceBookForm(_) if !targets.is_empty() => {
            return Err("create has no targets".into());
        }
        Command::ChannelDuplicateForm(_) if targets.len() != 1 => {
            return Err("duplicate requires one source channel".into());
        }
        Command::ChannelEndpointsForm(v) => {
            crate::wiring_channel_ext::validate_endpoints(&v.0.endpoints)
                .map_err(|_| "invalid endpoints")?
        }
        Command::ChannelOrderingForm(v) => {
            if v.0.channels.is_empty() {
                return Err("ordering requires channels".into());
            }
        }
        Command::ModelBatchForm(v) => {
            if !targets.is_empty() || v.models.is_empty() {
                return Err("model batch creates require models and no targets".into());
            }
            for v in &v.models {
                validate_command(&Command::ModelCreateForm(v.clone()), &[])?;
            }
        }
        Command::SettingForm(_) | Command::MappingAutomation(_) if !targets.is_empty() => {
            return Err("settings forms have no object targets".into());
        }
        Command::ProjectProfilesForm(v) => {
            if targets.is_empty() {
                return Err("Project targets are required".into());
            }
            crate::wiring_postgres_project_role::validate_project_profiles(&v.0)?;
        }
        Command::KeyTemplateCreateForm(_) | Command::ChannelTemplateCreateForm(_)
            if !targets.is_empty() =>
        {
            return Err("create has no targets".into());
        }
        Command::KeyTemplateUpdateForm(_) | Command::ChannelTemplateUpdateForm(_)
            if targets.is_empty() =>
        {
            return Err("template targets are required".into());
        }
        Command::KeyCreateForm(v) => {
            if !targets.is_empty()
                || v.0.name.trim().is_empty()
                || v.0.key_type == Some(conduit_admin_graphql::apikey::APIKeyType::Noauth)
            {
                return Err("invalid key creation".into());
            }
            if let Some(p) = &v.0.profiles {
                let p = crate::wiring_apikey::profiles_input_to_core(p.clone())
                    .map_err(|_| "invalid key profiles")?;
                conduit_services::apikey_service::validate_all_profiles(&p)
                    .map_err(|_| "invalid key profiles")?;
            }
        }
        Command::KeyUpdateForm(_)
        | Command::KeyProfilesForm(_)
        | Command::StorageUpdateForm(_)
        | Command::ProtectionUpdateForm(_)
            if targets.is_empty() =>
        {
            return Err("targets are required".into());
        }
        Command::KeyProfilesForm(v) => {
            let p = crate::wiring_apikey::profiles_input_to_core(v.0.clone())
                .map_err(|_| "invalid key profiles")?;
            conduit_services::apikey_service::validate_all_profiles(&p)
                .map_err(|_| "invalid key profiles")?;
        }
        Command::StorageCreateForm(_) if !targets.is_empty() => {
            return Err("create has no targets".into());
        }
        Command::ProtectionCreateForm(v) => {
            if !targets.is_empty() {
                return Err("create has no targets".into());
            }
            crate::wiring_prompt::validate_rule_settings(
                &v.0.pattern,
                &crate::wiring_prompt::protection_settings_input_to_core(v.0.settings.clone()),
            )
            .map_err(|_| "invalid protection settings")?;
        }
        Command::ModelCreateForm(v) => {
            if !targets.is_empty() || v.0.model_id.trim().is_empty() || v.0.name.trim().is_empty() {
                return Err("invalid model creation".into());
            }
        }
        Command::ModelUpdateForm(_) if targets.is_empty() => {
            return Err("model targets are required".into());
        }
        Command::MemberAddForm(v) => {
            validate_scopes(v.0.scopes.as_deref().unwrap_or_default(), true)?
        }
        Command::MemberUpdateForm(v) => {
            validate_scopes(v.0.scopes.as_deref().unwrap_or_default(), true)?
        }
        Command::UserCreateForm(v) => {
            if !targets.is_empty() {
                return Err("create has no targets".into());
            }
            conduit_admin_graphql::user::validate_create_user_input(&v.0)
                .map_err(|_| "invalid user form")?;
        }
        Command::UserUpdateForm(v) => {
            if targets.is_empty() {
                return Err("user targets are required".into());
            }
            conduit_admin_graphql::user::validate_update_user_input(&v.0)
                .map_err(|_| "invalid user form")?;
        }
        Command::ProjectCreateForm(v) => {
            if !targets.is_empty() || v.0.name.trim().is_empty() {
                return Err("new project requires name and no targets".into());
            }
        }
        Command::ProjectUpdateForm(_) | Command::RoleUpdateForm(_) if targets.is_empty() => {
            return Err("targets are required".into());
        }
        Command::RoleCreateForm(v) => {
            use conduit_admin_graphql::role::RoleLevel;
            let project = v.0.level == Some(RoleLevel::Project);
            if !targets.is_empty()
                || v.0.name.trim().is_empty()
                || project != v.0.project_id.is_some()
            {
                return Err("invalid role form".into());
            }
            validate_scopes(v.0.scopes.as_deref().unwrap_or_default(), project)?;
        }
        Command::ChannelBulkCreateForm(v) => {
            if !targets.is_empty()
                || v.0.api_keys.is_empty()
                || v.0.base_url.as_ref().is_none_or(|s| s.is_empty())
            {
                return Err("invalid channel bulk form".into());
            }
        }
        Command::ChannelImportForm(v) => {
            if !targets.is_empty()
                || v.0.channels.is_empty()
                || v.0.channels.iter().any(|c| {
                    !crate::wiring_channel_ext::is_valid_channel_type(&c.channel_type)
                        || c.name.trim().is_empty()
                        || c.base_url.as_ref().is_none_or(|s| s.is_empty())
                        || c.api_key.as_ref().is_none_or(|s| s.is_empty())
                })
            {
                return Err("invalid channel import form".into());
            }
        }
        Command::ChannelCreateForm(v) => {
            if !targets.is_empty() {
                return Err("create has no targets".into());
            }
            crate::wiring_channel_crud::prepare_channel_create(Some(1), v.0.clone())
                .map_err(|_| "invalid channel form")?;
        }
        Command::ChannelUpdateForm(_) if targets.is_empty() => {
            return Err("channel targets are required".into());
        }
        Command::ChannelBatch(v) => {
            if !targets.is_empty() || v.channels.is_empty() {
                return Err("batch creates require channels and no existing targets".into());
            }
            for channel in &v.channels {
                validate_command(&Command::Channel(channel.clone()), &[])?;
            }
        }
        Command::ModelBatch(v) => {
            if !targets.is_empty() || v.models.is_empty() {
                return Err("batch creates require models and no existing targets".into());
            }
            for model in &v.models {
                validate_command(&Command::Model(model.clone()), &[])?;
            }
        }
        Command::PublicModelRoutes(v) => {
            if !targets.is_empty() || v.deployment_ids.is_empty() {
                return Err("new public model requires routes and no existing targets".into());
            }
            if v.deployment_ids.len() > 1 && !v.confirm_compatibility {
                return Err("route compatibility confirmation is required".into());
            }
            if v.deployment_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != v.deployment_ids.len()
            {
                return Err("duplicate deployment route".into());
            }
            validate_command(&Command::Model(v.model.clone()), &[])?;
        }
        Command::ChannelDuplicate(v) => {
            if targets.len() != 1 || v.name.trim().is_empty() {
                return Err("duplicate requires one source and a new name".into());
            }
        }
        Command::ChannelKeys(_)
        | Command::TemplateApply(_)
        | Command::TemplateClear(_)
        | Command::OverrideTemplateDelete(_)
        | Command::KeyTemplateDelete(_)
        | Command::KeyTemplateLoad(_)
        | Command::ProtectionDelete(_)
            if targets.is_empty() =>
        {
            return Err("targets are required".into());
        }
        Command::OverrideTemplate(v) => {
            if let Some(ops) = &v.header_override_operations {
                crate::wiring_channel_override_template::validate_header_ops(ops)
                    .map_err(|_| "invalid header overrides")?;
            }
            if let Some(ops) = &v.body_override_operations {
                crate::wiring_channel_override_template::validate_body_ops(ops)
                    .map_err(|_| "invalid body overrides")?;
            }
        }
        Command::Protection(v) => {
            status(&v.status, &["enabled", "disabled", "archived"])?;
            if let Some(pattern) = &v.pattern {
                regex::Regex::new(pattern).map_err(|_| "invalid protection regex")?;
            }
            if let Some(settings) = &v.settings {
                if !matches!(settings.action.as_str(), "mask" | "reject") {
                    return Err("invalid protection action".into());
                }
            }
        }
        Command::Storage(v) => {
            status(&v.status, &["active", "archived"])?;
            if v.r#type
                .as_ref()
                .is_some_and(|v| !matches!(v.as_str(), "database" | "fs" | "s3" | "gcs" | "webdav"))
            {
                return Err("invalid storage type".into());
            }
        }
        Command::PriceTier(v) => {
            if v.multiplier_ppm.is_some_and(|v| v <= 0) {
                return Err("price multiplier must be positive".into());
            }
        }
        Command::Grant(v) => {
            if v.source_type.as_ref().is_some_and(|v| v != "manual") {
                return Err("administrative grants require manual source".into());
            }
        }
        Command::Entitlement(v) => {
            if v.effect
                .as_ref()
                .is_some_and(|v| !matches!(v.as_str(), "allow" | "block"))
            {
                return Err("invalid entitlement effect".into());
            }
        }
        Command::Channel(v) => {
            status(&v.status, &["enabled", "disabled", "archived"])?;
            if targets.is_empty()
                && (v.name.as_ref().is_none_or(|s| s.trim().is_empty())
                    || v.r#type.is_none()
                    || v.credentials.is_none())
            {
                return Err("channel name, type and credentials are required".into());
            }
        }
        Command::Model(v) => {
            status(&v.status, &["enabled", "disabled", "archived"])?;
            if targets.is_empty()
                && (v.model_id.as_ref().is_none_or(|s| s.trim().is_empty())
                    || v.name.is_none()
                    || v.developer.is_none())
            {
                return Err("model ID, name and developer are required".into());
            }
        }
        Command::Key(v) => {
            status(&v.status, &["enabled", "disabled", "archived"])?;
            if v.r#type
                .as_ref()
                .is_some_and(|s| !matches!(s.as_str(), "user" | "service_account"))
            {
                return Err("invalid key type".into());
            }
            if let Some(profiles) = &v.profiles {
                conduit_services::apikey_service::validate_all_profiles(profiles)
                    .map_err(|_| "invalid key profiles")?;
            }
            if targets.is_empty()
                && (v.user_id.is_none()
                    || v.project_id.is_none()
                    || v.name.as_ref().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("key name, member and Project are required".into());
            }
        }
        Command::Project(v) => status(&v.status, &["active", "archived"])?,
        Command::User(v) => {
            status(&v.status, &["activated", "deactivated"])?;
            if let Some(scopes) = &v.scopes {
                validate_scopes(scopes, false)?;
            }
        }
        Command::Role(v) => {
            if v.level
                .as_ref()
                .is_some_and(|s| !matches!(s.as_str(), "project" | "system"))
            {
                return Err("invalid role level".into());
            }
            if let Some(scopes) = &v.scopes {
                validate_scopes(scopes, v.level.as_deref() == Some("project"))?;
            }
        }
        Command::ChannelDelete(_)
        | Command::ModelDelete(_)
        | Command::ProjectDelete(_)
        | Command::UserDelete(_)
        | Command::RoleDelete(_)
        | Command::GroupDelete(_)
        | Command::SubscriptionCancel(_)
        | Command::Supply(_)
            if targets.is_empty() =>
        {
            return Err("targets are required".into());
        }
        Command::RedemptionCreate(v) => {
            conduit_admin_graphql::billing::validate_create_credit_redemption_codes_input(v)
                .map_err(|_| "invalid redemption issuance")?
        }
        Command::SubscriptionRefresh(_)
        | Command::SubscriptionPause(_)
        | Command::SubscriptionResume(_)
        | Command::SubscriptionRenew(_)
        | Command::SubscriptionAutoRenew(_)
        | Command::RedemptionRevoke(_)
            if targets.is_empty() =>
        {
            return Err("targets are required".into());
        }
        Command::Setting(v) => validate_setting(&v.key, &v.value)?,
        Command::SpendLimit(v) => {
            if let Some(value) = &v.limit {
                let amount = value
                    .parse::<rust_decimal::Decimal>()
                    .map_err(|_| "invalid Credits limit")?;
                if amount < rust_decimal::Decimal::ZERO || amount.scale() > 6 {
                    return Err(
                        "limit must be nonnegative Credits with at most six decimal places".into(),
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn write_row<T: Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    input: &T,
    targets: &[String],
) -> Result<Vec<String>, String> {
    let mut value = serde_json::to_value(input).map_err(|_| "invalid command")?;
    if targets.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        value["created_at"] = json!(now);
        value["updated_at"] = json!(now);
    }
    let fields: Vec<&str> = value
        .as_object()
        .ok_or("invalid command")?
        .keys()
        .map(String::as_str)
        .collect();
    if fields.is_empty() {
        return Err("command is empty".into());
    }
    // Fields came exclusively from a deny_unknown_fields DTO, and table names
    // are fixed by the action enum. All values use PostgreSQL bind parameters.
    let columns = fields
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(",");
    let mut result = Vec::new();
    if targets.is_empty() {
        let sql = format!(
            "INSERT INTO {table} ({columns}) SELECT {columns} FROM jsonb_populate_record(NULL::{table},$1) RETURNING id::text"
        );
        result.push(
            sqlx::query_scalar(&sql)
                .bind(sqlx::types::Json(value))
                .fetch_one(&mut **tx)
                .await
                .map_err(|_| "VALIDATION_FAILED: domain insert rejected")?,
        );
    } else {
        let set = fields
            .iter()
            .map(|f| format!("\"{f}\"=(jsonb_populate_record(NULL::{table},$1)).\"{f}\""))
            .collect::<Vec<_>>()
            .join(",");
        for target in targets {
            let sql = format!(
                "UPDATE {table} SET {set},updated_at=now() WHERE id::text=$2 RETURNING id::text"
            );
            result.push(
                sqlx::query_scalar(&sql)
                    .bind(sqlx::types::Json(value.clone()))
                    .bind(target)
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|_| "VALIDATION_FAILED: domain update rejected")?
                    .ok_or("SOURCE_CHANGED: target missing")?,
            );
        }
    }
    Ok(result)
}
async fn remove(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    targets: &[String],
) -> Result<Vec<String>, String> {
    if targets.is_empty() {
        return Err("targets are required".into());
    }
    let status = if table == "users" {
        "deactivated"
    } else {
        "archived"
    };
    for target in targets {
        sqlx::query(&format!("UPDATE {table} SET status='{status}',deleted_at=EXTRACT(EPOCH FROM now())::bigint,updated_at=now() WHERE id::text=$1 AND deleted_at=0")).bind(target).execute(&mut **tx).await.map_err(|_|"domain removal failed")?;
    }
    Ok(targets.to_vec())
}
pub(crate) async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    actor: i64,
    _policy: &Policy,
) -> Result<Vec<Value>, String> {
    check(tx, id, Some(actor)).await?;
    let rows=sqlx::query("SELECT action,private_payload FROM governed_change_commands WHERE change_set_id=$1 ORDER BY item_key").bind(id).fetch_all(&mut **tx).await.map_err(|_|"command lookup failed")?;
    let maker: Option<i64> = sqlx::query_scalar("SELECT created_by FROM change_sets WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| "maker lookup failed")?;
    let mut result = Vec::new();
    for row in rows {
        let action: String = row.get("action");
        let value: Value = row.get("private_payload");
        let targets: Vec<String> = decode(value["targets"].clone())?;
        let command = command(&action, value["payload"].clone())?;
        let ids=match command {
            Command::ChannelDuplicateForm(v)=>{let (name,input)=crate::wiring_channel_ext::ChannelExtMutationAdapter::prepare_create_from_gql_input(v.0).map_err(|_|"invalid duplicate form")?;let row=conduit_db::PgChannelRepo::create_channel_in_tx(tx,input).await.map_err(|_|"channel duplication failed")?;crate::wiring_postgres_change_sets::publish_copied_provider_prices(tx,actor,Some(id),targets[0].parse().map_err(|_|"invalid source channel")?,row.id.parse().map_err(|_|"invalid duplicate channel")?,&name).await?;vec![row.id]},
            Command::ChannelEndpointsForm(v)=>write_fixed_value(tx,"channels",json!({"endpoints":v.0.endpoints.into_iter().map(crate::wiring_channel_ext::endpoint_input_to_json).collect::<Vec<_>>()}),&targets).await?,
            Command::ChannelOrderingForm(v)=>{let mut ids=Vec::new();for v in v.0.channels{ids.extend(write_fixed_value(tx,"channels",json!({"ordering_weight":v.ordering_weight}),&[numeric(&v.id)?.to_string()]).await?);}ids},
            Command::ModelBatchForm(v)=>{let mut ids=Vec::new();for v in v.models{let v=v.0;let cols=crate::conv::create_model_columns(&v);ids.extend(write_row(tx,"models",&ModelWrite{model_id:Some(v.model_id),developer:Some(v.developer),name:Some(v.name),r#type:Some(cols.model_type),r#group:Some(v.group),icon:Some(v.icon),model_card:Some(decode(cols.model_card)?),settings:Some(decode(cols.settings)?),remark:v.remark,status:Some("disabled".into())},&[]).await?);}ids},
            Command::MappingAutomation(v)=>{sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES('channel_model_mapping_automation_enabled',$1,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,updated_at=now(),deleted_at=0").bind(if v.0.enabled{"true"}else{"false"}).execute(&mut **tx).await.map_err(|_|"mapping automation update failed")?;vec!["channel_model_mapping_automation_enabled".into()]},
            Command::AliasesForm(v)=>{crate::wiring_postgres_commercialization::apply_channel_model_mappings_in_tx(tx,v.0).await.map_err(|_|"channel aliases publication rejected")?;targets.clone()},
            Command::RouteForm(v)=>vec![crate::wiring_postgres_commercialization::upsert_model_route_in_tx(tx,v.0).await.map_err(|_|"route publication rejected")?.to_string()],
            Command::PublicModelForm(v)=>vec![numeric(&crate::wiring_postgres_commercialization::create_public_model_with_routes_in_tx(tx,v.0).await.map_err(|_|"public model publication rejected")?.model.id)?.to_string()],
            Command::PriceBookForm(v)=>vec![crate::wiring_postgres_commercialization::create_price_book_in_tx(tx,Some(actor),v.0).await.map_err(|_|"price book publication rejected")?.to_string()],
            Command::SettingForm(v)=>{v.apply(tx,actor).await?;vec![v.key().into()]},
            Command::ProjectProfilesForm(v)=>{let mut ids=Vec::new();for target in &targets{let previous:Value=sqlx::query_scalar("SELECT profiles FROM projects WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"Project is missing")?;let previous=decode(previous)?;let mut input=v.0.clone();crate::wiring_postgres_project_role::preserve_project_channel_selectors(&mut input,&previous)?;ids.extend(write_fixed_value(tx,"projects",json!({"profiles":crate::wiring_postgres_project_role::profiles_input_to_json(&input)}),&[target.clone()]).await?);}ids},
            Command::KeyTemplateCreateForm(v)=>{let mut profile=crate::wiring_profile_template::profile_input_to_core(v.profile.0);profile.name=v.input.0.name.clone();write_row(tx,"api_key_profile_templates",&KeyTemplateWrite{project_id:Some(numeric(&v.input.0.project_id)?),name:Some(v.input.0.name),description:v.input.0.description,profile:Some(profile)},&[]).await?},
            Command::KeyTemplateUpdateForm(v)=>{let mut ids=Vec::new();for target in &targets{let name:String=sqlx::query_scalar("SELECT name FROM api_key_profile_templates WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"template is missing")?;let profile=v.profile.clone().map(|profile|{let mut p=crate::wiring_profile_template::profile_input_to_core(profile.0);p.name=v.input.0.name.clone().unwrap_or(name);p});ids.extend(write_row(tx,"api_key_profile_templates",&KeyTemplateWrite{project_id:None,name:v.input.0.name.clone(),description:v.input.0.description.clone(),profile},&[target.clone()]).await?);}ids},
            Command::ChannelTemplateCreateForm(v)=>{let v=v.0;let headers=crate::wiring_channel_override_template::convert_ops(v.header_override_operations.unwrap_or_default());let body=crate::wiring_channel_override_template::convert_ops(v.body_override_operations.unwrap_or_default());crate::wiring_channel_override_template::validate_header_ops(&headers).map_err(|_|"invalid header operations")?;crate::wiring_channel_override_template::validate_body_ops(&body).map_err(|_|"invalid body operations")?;write_fixed_value(tx,"channel_override_templates",json!({"user_id":maker.ok_or("template maker is required")?,"name":v.name,"description":v.description,"header_override_operations":headers,"body_override_operations":body}),&[]).await?},
            Command::ChannelTemplateUpdateForm(v)=>{let v=v.0;let mut ids=Vec::new();for target in &targets{let row=sqlx::query("SELECT header_override_operations,body_override_operations FROM channel_override_templates WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"template is missing")?;let mut value=json!({});if let Some(name)=&v.name{value["name"]=json!(name);}if v.clear_description==Some(true){value["description"]=Value::Null;}else if let Some(description)=&v.description{value["description"]=json!(description);}if let Some(headers)=crate::wiring_channel_override_template::merged_update_ops(decode(row.get("header_override_operations"))?,v.header_override_operations.clone(),v.append_header_override_operations.clone(),v.clear_header_override_operations.unwrap_or(false)){crate::wiring_channel_override_template::validate_header_ops(&headers).map_err(|_|"invalid header operations")?;value["header_override_operations"]=json!(headers);}if let Some(body)=crate::wiring_channel_override_template::merged_update_ops(decode(row.get("body_override_operations"))?,v.body_override_operations.clone(),v.append_body_override_operations.clone(),v.clear_body_override_operations.unwrap_or(false)){crate::wiring_channel_override_template::validate_body_ops(&body).map_err(|_|"invalid body operations")?;value["body_override_operations"]=json!(body);}ids.extend(write_fixed_value(tx,"channel_override_templates",value,&[target.clone()]).await?);}ids},
            Command::KeyCreateForm(v)=>{let v=v.0;let user=maker.ok_or("billing member is required")?;let key=conduit_auth::apikey::generate_api_key("conduit").map_err(|_|"key generation failed")?;let kind=if v.key_type==Some(conduit_admin_graphql::apikey::APIKeyType::ServiceAccount){"service_account"}else{"user"};let scopes=if kind=="user"{vec!["read_channels".into(),"write_requests".into()]}else{v.scopes.unwrap_or_default()};let profiles=v.profiles.map(crate::wiring_apikey::profiles_input_to_core).transpose().map_err(|_|"invalid key profiles")?;let mut value=json!({"name":v.name,"user_id":user,"project_id":numeric(&v.project_id)?,"type":kind,"status":"enabled","scopes":scopes,"key":key});if let Some(p)=profiles{value["profiles"]=json!(p);}let ids=write_fixed_key(tx,value,&[]).await?;store_secrets(tx,id,json!({"keys":[{"id":ids[0],"key":key}]})).await?;ids},
            Command::KeyUpdateForm(v)=>{let mut ids=Vec::new();for target in &targets{let row=sqlx::query("SELECT type,scopes FROM api_keys WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"key is missing")?;let kind:String=row.get("type");let input=&v.0;let touched=input.scopes.as_ref().is_some_and(|s|!s.is_empty())||input.append_scopes.as_ref().is_some_and(|s|!s.is_empty())||input.clear_scopes==Some(true);if kind=="noauth"||kind=="user"&&touched{return Err("key scopes are immutable".into());}let mut value=json!({});if let Some(name)=&input.name{value["name"]=json!(name);}if touched{let mut scopes:Vec<String>=decode(row.get("scopes"))?;if let Some(set)=&input.scopes{if !set.is_empty(){scopes=set.clone();}}scopes.extend(input.append_scopes.clone().unwrap_or_default());if input.clear_scopes==Some(true){scopes.clear();}value["scopes"]=json!(scopes);}ids.extend(write_fixed_key(tx,value,&[target.clone()]).await?);}ids},
            Command::KeyProfilesForm(v)=>{let profiles=crate::wiring_apikey::profiles_input_to_core(v.0).map_err(|_|"invalid key profiles")?;write_fixed_key(tx,json!({"profiles":profiles}),&targets).await?},
            Command::StorageCreateForm(v)=>write_fixed_value(tx,"data_storages",crate::wiring_data_storage::governed_create(v.0),&[]).await?,
            Command::StorageUpdateForm(v)=>{let mut ids=Vec::new();for target in &targets{let existing:Value=sqlx::query_scalar("SELECT settings FROM data_storages WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"storage is missing")?;ids.extend(write_fixed_value(tx,"data_storages",crate::wiring_data_storage::governed_update(v.0.clone(),&existing),&[target.clone()]).await?);}ids},
            Command::ProtectionCreateForm(v)=>{let v=v.0;write_row(tx,"prompt_protection_rules",&ProtectionWrite{name:Some(v.name),description:v.description,pattern:Some(v.pattern),settings:Some(crate::wiring_prompt::protection_settings_input_to_core(v.settings)),status:Some("disabled".into())},&[]).await?},
            Command::ProtectionUpdateForm(v)=>{let v=v.0;for target in &targets{let row=sqlx::query("SELECT pattern,settings FROM prompt_protection_rules WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"protection rule is missing")?;let settings=v.settings.clone().map(crate::wiring_prompt::protection_settings_input_to_core).unwrap_or(decode(row.get("settings"))?);crate::wiring_prompt::validate_rule_settings(v.pattern.as_deref().unwrap_or(&row.get::<String,_>("pattern")),&settings).map_err(|_|"invalid protection settings")?;}write_row(tx,"prompt_protection_rules",&ProtectionWrite{name:v.name,description:v.description,pattern:v.pattern,settings:v.settings.map(crate::wiring_prompt::protection_settings_input_to_core),status:v.status.map(|v|crate::wiring_prompt::rule_status_to_wire(v).into())},&targets).await?},
            Command::MemberAddForm(v)=>{crate::wiring_postgres_user::add_user_to_project_in_tx(tx,v.0).await.map_err(|_|"membership creation rejected")?;targets.clone()},
            Command::MemberUpdateForm(v)=>{crate::wiring_postgres_user::update_project_user_in_tx(tx,v.0).await.map_err(|_|"membership update rejected")?;targets.clone()},
            Command::MemberRemoveForm(v)=>{crate::wiring_postgres_user::remove_user_from_project_in_tx(tx,v.0).await.map_err(|_|"membership removal rejected")?;targets.clone()},
            Command::ModelCreateForm(v)=>{let v=v.0;let cols=crate::conv::create_model_columns(&v);write_row(tx,"models",&ModelWrite{model_id:Some(v.model_id),developer:Some(v.developer),name:Some(v.name),r#type:Some(cols.model_type),r#group:Some(v.group),icon:Some(v.icon),model_card:Some(decode(cols.model_card)?),settings:Some(decode(cols.settings)?),remark:v.remark,status:Some("disabled".into())},&[]).await?},
            Command::ModelUpdateForm(v)=>{let v=v.0;let row=ModelWrite{model_id:v.model_id,developer:v.developer,name:v.name,r#type:v.model_type.map(|t|crate::wiring::gql_model_type_to_wire(t).into()),r#group:v.group,icon:v.icon,model_card:v.model_card.as_ref().map(crate::conv::model_card_input_to_json).map(decode).transpose()?,settings:v.settings.as_ref().map(crate::conv::model_settings_input_to_json).map(decode).transpose()?,remark:v.remark,status:v.status.map(|t|crate::wiring::model_status_to_wire(t).into())};let mut value=serde_json::to_value(row).map_err(|_|"invalid model update")?;if v.clear_remark==Some(true){value["remark"]=Value::Null;}write_fixed_value(tx,"models",value,&targets).await?},
            Command::UserCreateForm(v)=>{let prepared=v.0.password.clone();let user=crate::wiring_postgres_user::create_user_prepared_in_tx(tx,v.0,prepared).await.map_err(|_|"user creation rejected")?;vec![numeric(&user.id)?.to_string()]},
            Command::UserUpdateForm(v)=>{let mut ids=Vec::new();for target in &targets {let mut input=v.0.clone();let prepared=input.password.take();let user=crate::wiring_postgres_user::update_user_in_tx(tx,conduit_auth::DEFAULT_BCRYPT_COST,target,input).await.map_err(|_|"user update rejected")?;if let Some(password)=prepared{sqlx::query("UPDATE users SET password=$1,updated_at=now() WHERE id::text=$2 AND deleted_at=0").bind(password).bind(target).execute(&mut **tx).await.map_err(|_|"password update rejected")?;}ids.push(numeric(&user.id)?.to_string());}ids},
            Command::ProjectCreateForm(v)=>vec![crate::wiring_postgres_project_role::create_project_in_tx(tx,v.0).await.map_err(|_|"project creation rejected")?.to_string()],
            Command::ProjectUpdateForm(v)=>write_row(tx,"projects",&ProjectWrite{name:v.0.name,description:v.0.description,status:None,profiles:None},&targets).await?,
            Command::RoleCreateForm(v)=>{let project=v.0.level==Some(conduit_admin_graphql::role::RoleLevel::Project);write_row(tx,"roles",&RoleWrite{name:Some(v.0.name),level:Some(if project{"project"}else{"system"}.into()),project_id:v.0.project_id.as_ref().map(numeric).transpose()?,scopes:Some(v.0.scopes.unwrap_or_default())},&[]).await?},
            Command::RoleUpdateForm(v)=>{for target in &targets {let project:bool=sqlx::query_scalar("SELECT level='project' FROM roles WHERE id::text=$1 AND deleted_at=0").bind(target).fetch_one(&mut **tx).await.map_err(|_|"role is missing")?;if let Some(scopes)=&v.0.scopes{validate_scopes(scopes,project)?;}}write_row(tx,"roles",&RoleWrite{name:v.0.name,level:None,project_id:None,scopes:v.0.scopes},&targets).await?},
            Command::ChannelBulkCreateForm(v)=>{let v=v.0;let mut names:std::collections::HashSet<String>=sqlx::query_scalar("SELECT name FROM channels WHERE deleted_at=0").fetch_all(&mut **tx).await.map_err(|_|"channel names unavailable")?.into_iter().collect();let mut index=1;let mut ids=Vec::new();for key in &v.api_keys{let mut name=format!("{} - ({index})",v.name);while names.contains(&name){index+=1;name=format!("{} - ({index})",v.name);}index+=1;names.insert(name.clone());let input=channel_bulk_input(crate::wiring_channel_crud::channel_type_to_wire(v.channel_type).into(),name,v.base_url.clone(),key.clone(),v.supported_models.clone(),v.default_test_model.clone(),v.settings.clone(),v.policies.clone(),v.tags.clone().filter(|v|!v.is_empty()).unwrap_or_else(||vec![v.name.clone()]),v.auto_sync_supported_models,v.ordering_weight,v.remark.clone());ids.push(conduit_db::PgChannelRepo::create_channel_in_tx(tx,input).await.map_err(|_|"atomic bulk channel creation failed")?.id);}ids},
            Command::ChannelImportForm(v)=>{let mut ids=Vec::new();for v in v.0.channels{let input=channel_bulk_input(v.channel_type,v.name,v.base_url,v.api_key.ok_or("import key is required")?,v.supported_models,v.default_test_model,None,None,vec![],None,None,None);ids.push(conduit_db::PgChannelRepo::create_channel_in_tx(tx,input).await.map_err(|_|"atomic channel import failed")?.id);}ids},
            Command::ChannelCreateForm(v)=>{let (input,new_api,operation)=crate::wiring_channel_crud::prepare_channel_create(maker,v.0).map_err(|_|"invalid channel form")?;let mut intent=input.clone();intent.created_at.clear();let fingerprint=format!("{:x}",Sha256::digest(format!("{:?}:{:?}",maker,intent).as_bytes()));let row=conduit_db::PgChannelRepo::create_channel_in_tx(tx,input).await.map_err(|_|"channel creation failed")?;if new_api{let stages=crate::wiring_postgres_channel_discovery::initial_results(&[conduit_admin_graphql::channel_discovery::ChannelDiscoveryStage::Models,conduit_admin_graphql::channel_discovery::ChannelDiscoveryStage::Prices],None);sqlx::query("INSERT INTO channel_operation_receipts(operation_id,channel_id,operation_type,origin,created_by,request_fingerprint,state,stage_results,created_at,updated_at) VALUES($1,$2,'NEW_API_DISCOVERY','CHANNEL_CREATE',$3,$4,'NOT_STARTED',$5,now(),now())").bind(operation.ok_or("NEWAPI operation required")?).bind(row.id.parse::<i64>().map_err(|_|"invalid channel ID")?).bind(maker).bind(fingerprint).bind(sqlx::types::Json(stages)).execute(&mut **tx).await.map_err(|_|"channel receipt creation failed")?;}vec![row.id]},
            Command::ChannelUpdateForm(v)=>{let mut ids=Vec::new();for target in &targets{let existing=conduit_db::PgChannelRepo::find_channel_in_tx(tx,target).await.map_err(|_|"channel lookup failed")?.ok_or("channel is missing")?;let input=crate::wiring_channel_crud::prepare_channel_update(v.0.clone(),&existing).map_err(|_|"invalid channel form")?;ids.push(conduit_db::PgChannelRepo::update_channel_in_tx(tx,target,input).await.map_err(|_|"channel update failed")?.id);}ids},
            Command::ChannelBatch(v)=>{let mut ids=Vec::new();for channel in v.channels{ids.extend(write_row(tx,"channels",&channel,&[]).await?);}ids},
            Command::ModelBatch(v)=>{let mut ids=Vec::new();for mut model in v.models{model.icon.get_or_insert_with(String::new);model.r#group.get_or_insert_with(||model.developer.clone().unwrap_or_default());model.model_card.get_or_insert_with(Default::default);model.settings.get_or_insert_with(Default::default);ids.extend(write_row(tx,"models",&model,&[]).await?);}ids},
            Command::ChannelDuplicate(v)=>{let source=&targets[0];let new_id:i64=sqlx::query_scalar("INSERT INTO channels(type,name,base_url,default_test_model,status,credentials,settings,supported_models,manual_models,policies,endpoints,ordering_weight,tags,auto_sync_supported_models,auto_sync_model_pattern) SELECT type,$2,base_url,default_test_model,'disabled',credentials,settings,supported_models,manual_models,policies,endpoints,ordering_weight,tags,auto_sync_supported_models,auto_sync_model_pattern FROM channels WHERE id::text=$1 AND deleted_at=0 RETURNING id").bind(source).bind(v.name).fetch_one(&mut **tx).await.map_err(|_|"channel duplication failed")?;if v.include_prices{let name:String=sqlx::query_scalar("SELECT name FROM channels WHERE id=$1").bind(new_id).fetch_one(&mut **tx).await.map_err(|_|"duplicate name unavailable")?;crate::wiring_postgres_change_sets::publish_copied_provider_prices(tx,actor,Some(id),source.parse().map_err(|_|"invalid source channel")?,new_id,&name).await?;}vec![new_id.to_string()]},
            Command::ChannelKeys(v)=>{for target in &targets {apply_channel_keys(tx,target,&v).await?;}targets.clone()},
            Command::OverrideTemplate(v)=>{let mut value=serde_json::to_value(&v).map_err(|_|"invalid template")?;if targets.is_empty(){value["user_id"]=json!(maker.ok_or("template maker is required")?);}write_fixed_value(tx,"channel_override_templates",value,&targets).await?},
            Command::OverrideTemplateDelete(_)=>soft_delete_only(tx,"channel_override_templates",&targets).await?,
            Command::TemplateApply(v)=>{let data:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM channel_override_templates t WHERE id=$1 AND deleted_at=0 AND user_id=$2").bind(v.template_id).bind(maker.ok_or("template maker is required")?).fetch_one(&mut **tx).await.map_err(|_|"template is outside current ownership")?;let headers:Vec<conduit_core::objects::overrides::OverrideOperation>=decode(data["header_override_operations"].as_array().cloned().map(Value::Array).unwrap_or(json!([])))?;let body:Vec<conduit_core::objects::overrides::OverrideOperation>=decode(data["body_override_operations"].as_array().cloned().map(Value::Array).unwrap_or(json!([])))?;for target in &targets {let raw:Option<Value>=sqlx::query_scalar("SELECT settings FROM channels WHERE id::text=$1").bind(target).fetch_one(&mut **tx).await.map_err(|_|"channel setting lookup failed")?;let mut settings:conduit_core::objects::channel_settings::ChannelSettings=decode(raw.unwrap_or(json!({})))?;settings.header_override_operations=if v.replace{headers.clone()}else{crate::wiring_channel_override_template::merge_header_ops(settings.header_override_operations,&headers)};settings.body_override_operations=if v.replace{body.clone()}else{crate::wiring_channel_override_template::merge_body_ops(settings.body_override_operations,&body)};settings.override_headers.clear();settings.override_parameters.clear();sqlx::query("UPDATE channels SET settings=$1,updated_at=now() WHERE id::text=$2").bind(sqlx::types::Json(settings)).bind(target).execute(&mut **tx).await.map_err(|_|"template application failed")?;}targets.clone()},
            Command::TemplateClear(_)=>{for target in &targets {sqlx::query("UPDATE channels SET settings=COALESCE(settings,'{}'::jsonb)-ARRAY['bodyOverrideOperations','headerOverrideOperations','overrideHeaders','overrideParameters'],updated_at=now() WHERE id::text=$1").bind(target).execute(&mut **tx).await.map_err(|_|"template clear failed")?;}targets.clone()},
            Command::KeyTemplate(v)=>write_row(tx,"api_key_profile_templates",&v,&targets).await?,Command::KeyTemplateDelete(_)=>soft_delete_only(tx,"api_key_profile_templates",&targets).await?,
            Command::KeyTemplateLoad(v)=>{for target in &targets {let row=sqlx::query("SELECT t.profile,t.name,k.profiles FROM api_key_profile_templates t JOIN api_keys k ON k.project_id=t.project_id WHERE t.id=$1 AND k.id::text=$2 AND t.deleted_at=0 AND k.deleted_at=0 FOR UPDATE OF k").bind(v.template_id).bind(target).fetch_one(&mut **tx).await.map_err(|_|"template Project mismatch")?;let mut profile:conduit_core::objects::apikey::APIKeyProfile=decode(row.get::<Value,_>("profile"))?;let mut profiles:conduit_core::objects::apikey::APIKeyProfiles=decode(row.get::<Option<Value>,_>("profiles").unwrap_or(json!({})))?;let base=if profile.name.is_empty(){row.get::<String,_>("name")}else{profile.name.clone()};profile.name=conduit_services::apikey_service::resolve_profile_name_conflict(&profiles.profiles,&base);profiles.profiles.push(profile);conduit_services::apikey_service::validate_all_profiles(&profiles).map_err(|_|"invalid key profile")?;sqlx::query("UPDATE api_keys SET profiles=$1,updated_at=now() WHERE id::text=$2").bind(sqlx::types::Json(profiles)).bind(target).execute(&mut **tx).await.map_err(|_|"profile template load failed")?;}targets.clone()},
            Command::Protection(v)=>write_row(tx,"prompt_protection_rules",&v,&targets).await?,Command::ProtectionDelete(_)=>remove(tx,"prompt_protection_rules",&targets).await?,Command::Storage(v)=>{for target in &targets{let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM data_storages d WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"storage is missing")?;if v.r#type.as_ref().is_some_and(|v|json!(v)!=current["type"]){return Err("storage type is immutable".into());}if current["primary"]==true&&v.status.as_deref()==Some("archived"){return Err("primary storage cannot be archived".into());}}write_row(tx,"data_storages",&v,&targets).await?},Command::AccessPlan(v)=>write_row(tx,"access_plans",&v,&targets).await?,Command::PriceTier(v)=>write_row(tx,"price_tiers",&v,&targets).await?,Command::Grant(v)=>write_row(tx,"project_access_grants",&v,&targets).await?,Command::Entitlement(v)=>write_row(tx,"project_entitlement_overrides",&v,&targets).await?,Command::Adjustment(v)=>write_row(tx,"project_price_adjustments",&v,&targets).await?,
            Command::PublicModelRoutes(v)=>{let channels:Vec<i64>=sqlx::query_scalar("SELECT channel_id FROM upstream_model_deployments WHERE id=ANY($1) AND deleted_at=0 AND status='enabled'").bind(&v.deployment_ids).fetch_all(&mut **tx).await.map_err(|_|"deployment validation failed")?;if channels.len()!=v.deployment_ids.len()||channels.iter().copied().collect::<std::collections::BTreeSet<_>>().len()!=channels.len(){return Err("routes require enabled deployments from distinct channels".into());}let mut model=v.model;model.icon.get_or_insert_with(String::new);model.r#group.get_or_insert_with(||model.developer.clone().unwrap_or_default());model.model_card.get_or_insert_with(Default::default);model.settings.get_or_insert_with(Default::default);let ids=write_row(tx,"models",&model,&[]).await?;let model_id=ids[0].parse::<i64>().map_err(|_|"invalid model result")?;for deployment in v.deployment_ids{sqlx::query("INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES($1,$2,$3)").bind(model_id).bind(deployment).bind(if v.enabled{"enabled"}else{"disabled"}).execute(&mut **tx).await.map_err(|_|"new model route failed")?;}ids},
            Command::Channel(mut v)=>{if targets.is_empty(){v.default_test_model.get_or_insert_with(String::new);v.supported_models.get_or_insert_with(Vec::new);v.status.get_or_insert_with(||"disabled".into());v.settings.get_or_insert_with(Default::default);}write_row(tx,"channels",&v,&targets).await?},Command::Model(mut v)=>{if targets.is_empty(){v.icon.get_or_insert_with(String::new);v.r#group.get_or_insert_with(||v.developer.clone().unwrap_or_default());v.model_card.get_or_insert_with(Default::default);v.settings.get_or_insert_with(Default::default);}write_row(tx,"models",&v,&targets).await?},Command::Project(v)=>{if targets.is_empty(){let project=crate::wiring_postgres_project_role::create_project_in_tx(tx,conduit_admin_graphql::project::CreateProjectInput{name:v.name.clone().ok_or("Project name is required")?,description:v.description.clone(),status:None,user_ids:None}).await.map_err(|_|"Project creation rejected")?;write_row(tx,"projects",&v,&[project.to_string()]).await?}else{write_row(tx,"projects",&v,&targets).await?}},
            Command::User(v)=>{if let Some(scopes)=&v.scopes {validate_scopes(scopes,false)?;}if targets.is_empty(){let password=v.password.clone().ok_or("password is required")?;let user=crate::wiring_postgres_user::create_user_prepared_in_tx(tx,conduit_admin_graphql::user::CreateUserInput{email:v.email.clone().ok_or("email is required")?,password:password.clone(),status:v.status.as_ref().map(|s|if s=="activated"{conduit_admin_graphql::user::UserStatus::Activated}else{conduit_admin_graphql::user::UserStatus::Deactivated}),prefer_language:v.prefer_language,first_name:v.first_name,last_name:v.last_name,avatar:None,is_owner:None,scopes:v.scopes,project_ids:None,role_ids:None},password).await.map_err(|_|"user creation rejected")?;vec![numeric(&user.id)?.to_string()]}else{for target in &targets{if let Some(status)=&v.status{crate::wiring_postgres_user::update_user_status_in_tx(tx,target,if status=="activated"{conduit_admin_graphql::user::UserStatus::Activated}else{conduit_admin_graphql::user::UserStatus::Deactivated}).await.map_err(|_|"user status rejected")?;}}write_row(tx,"users",&v,&targets).await?}},
            Command::Role(v)=>{if targets.is_empty(){let project=v.level.as_deref()==Some("project");if project!=v.project_id.is_some(){return Err("project role requires Project and system role cannot have one".into());}validate_scopes(v.scopes.as_deref().unwrap_or_default(),project)?;}else{for target in &targets{let current:Value=sqlx::query_scalar("SELECT to_jsonb(r) FROM roles r WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"role is missing")?;if v.level.as_ref().is_some_and(|v|json!(v)!=current["level"])||v.project_id.is_some_and(|v|json!(v)!=current["project_id"]){return Err("role level and Project are immutable".into());}validate_scopes(v.scopes.as_deref().unwrap_or_default(),current["level"]=="project")?;}}write_row(tx,"roles",&v,&targets).await?},
            Command::ChannelDelete(_)=>remove(tx,"channels",&targets).await?,Command::ModelDelete(_)=>{let ids=remove(tx,"models",&targets).await?;sqlx::query("UPDATE model_routes SET status='disabled',updated_at=now() WHERE public_model_id::text=ANY($1)").bind(&targets).execute(&mut **tx).await.map_err(|_|"model route revocation failed")?;ids},Command::ProjectDelete(_)=>remove(tx,"projects",&targets).await?,Command::UserDelete(_)=>{for target in &targets{crate::wiring_postgres_user::delete_user_in_tx(tx,target).await.map_err(|_|"user deletion rejected")?;}targets.clone()},
            Command::RoleDelete(_)=>{for target in &targets {sqlx::query("UPDATE roles SET deleted_at=EXTRACT(EPOCH FROM now())::bigint,updated_at=now() WHERE id::text=$1").bind(target).execute(&mut **tx).await.map_err(|_|"role removal failed")?;}targets.clone()},
            Command::Supply(v)=>{apply_supply(tx,&v.supported_models,&targets).await?;targets.clone()},
            Command::Key(mut v)=>{if v.r#type.as_deref()==Some("noauth"){return Err("noauth keys cannot be governed".into());}if let Some(scopes)=&v.scopes{validate_scopes(scopes,false)?;}let mut value=serde_json::to_value(&v).map_err(|_|"invalid key command")?;if targets.is_empty(){v.user_id.ok_or("billing member is required")?;v.project_id.ok_or("Project is required")?;v.r#type.get_or_insert_with(||"user".into());v.status.get_or_insert_with(||"enabled".into());if v.r#type.as_deref()==Some("user"){v.scopes=Some(vec!["read_channels".into(),"write_requests".into()]);}value=serde_json::to_value(&v).map_err(|_|"invalid key command")?;let key=conduit_auth::apikey::generate_api_key("conduit").map_err(|_|"key generation failed")?;value["key"]=json!(key);}let generated=value.get("key").cloned();let ids=write_fixed_key(tx,value,&targets).await?;if let Some(key)=generated {store_secrets(tx,id,json!({"keys":[{"id":ids[0],"key":key}]})).await?;}ids},
            Command::KeyRotate(_)=>{let mut secrets=Vec::new();for target in &targets {let key=conduit_auth::apikey::generate_api_key("conduit").map_err(|_|"key generation failed")?;sqlx::query("UPDATE api_keys SET key=$1,updated_at=now() WHERE id::text=$2 AND type<>'noauth' AND deleted_at=0").bind(&key).bind(target).execute(&mut **tx).await.map_err(|_|"key rotation failed")?;secrets.push(json!({"id":target,"key":key}));}store_secrets(tx,id,json!({"keys":secrets})).await?;targets.clone()},
            Command::KeyDelete(_)=>remove(tx,"api_keys",&targets).await?,
            Command::Roles(v)=>{sqlx::query("DELETE FROM user_roles WHERE user_id=$1").bind(v.user_id).execute(&mut **tx).await.map_err(|_|"role assignment failed")?;for role in v.role_ids {let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM roles WHERE id=$1 AND deleted_at=0 AND (level='system' OR EXISTS(SELECT 1 FROM user_projects WHERE project_id=roles.project_id AND user_id=$2)))").bind(role).bind(v.user_id).fetch_one(&mut **tx).await.map_err(|_|"role scope lookup failed")?;if !exists{return Err("role scope does not belong to the target user".into());}sqlx::query("INSERT INTO user_roles(user_id,role_id,created_at,updated_at) VALUES($1,$2,now(),now())").bind(v.user_id).bind(role).execute(&mut **tx).await.map_err(|_|"role assignment failed")?;}vec![v.user_id.to_string()]},
            Command::Route(v)=>vec![crate::wiring_postgres_commercialization::upsert_model_route_in_tx(tx,conduit_admin_graphql::commercialization::UpsertModelRouteInput{id:targets.first().map(|v|v.clone().into()),public_model_id:v.public_model_id.to_string().into(),deployment_id:v.deployment_id.to_string().into(),status:Some(match v.status.as_str(){"enabled"=>conduit_admin_graphql::commercialization::CommercialStatus::Enabled,"disabled"=>conduit_admin_graphql::commercialization::CommercialStatus::Disabled,_=>return Err("invalid route status".into())}),confirm_compatibility:Some(v.confirm_compatibility)}).await.map_err(|_|"route publication rejected")?.to_string()],
            Command::Deployment(v)=>{if !matches!(v.status.as_str(),"enabled"|"disabled")||v.upstream_model_id.trim().is_empty(){return Err("invalid deployment".into());}let available:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM channels WHERE id=$1 AND deleted_at=0 AND (supported_models ? $2 OR manual_models ? $2))").bind(v.channel_id).bind(&v.upstream_model_id).fetch_one(&mut **tx).await.map_err(|_|"deployment source lookup failed")?;if !available{return Err("SOURCE_CHANGED: publish model supply before its deployment".into());}vec![sqlx::query_scalar::<_,i64>("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,variant,status,source) VALUES($1,$2,$3,$4,$5,'manual') ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET internal_name=EXCLUDED.internal_name,status=EXCLUDED.status,updated_at=now() RETURNING id").bind(v.channel_id).bind(v.upstream_model_id).bind(v.internal_name).bind(v.variant).bind(v.status).fetch_one(&mut **tx).await.map_err(|_|"deployment application failed")?.to_string()]},
            Command::Commercial(v)=>{for target in &targets {let value=serde_json::to_value(&v).map_err(|_|"invalid commercial profile")?;let fields=value.as_object().ok_or("invalid profile")?.keys().map(|k|format!("{k}=(jsonb_populate_record(NULL::project_commercial_profiles,$1)).{k}")).collect::<Vec<_>>().join(",");if fields.is_empty(){return Err("commercial profile is empty".into());}sqlx::query(&format!("UPDATE project_commercial_profiles SET {fields},updated_at=now() WHERE project_id::text=$2")).bind(sqlx::types::Json(value)).bind(target).execute(&mut **tx).await.map_err(|_|"commercial profile application failed")?;}targets.clone()},
            Command::Member(v)=>{let project=v.project_id.ok_or("project_id is required")?;let user=v.user_id.ok_or("user_id is required")?;let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_projects WHERE project_id=$1 AND user_id=$2)").bind(project).bind(user).fetch_one(&mut **tx).await.map_err(|_|"membership lookup failed")?;if exists{crate::wiring_postgres_user::update_project_user_in_tx(tx,conduit_admin_graphql::user::UpdateProjectUserInput{project_id:project.to_string().into(),user_id:user.to_string().into(),is_owner:v.is_owner,scopes:v.scopes,add_role_ids:None,remove_role_ids:None}).await.map_err(|_|"membership update rejected")?;}else{crate::wiring_postgres_user::add_user_to_project_in_tx(tx,conduit_admin_graphql::user::AddUserToProjectInput{project_id:project.to_string().into(),user_id:user.to_string().into(),is_owner:v.is_owner,scopes:v.scopes,role_ids:None}).await.map_err(|_|"membership creation rejected")?;}vec![project.to_string()]},
            Command::MemberRemove(v)=>{let project=v.project_id.ok_or("project_id is required")?;crate::wiring_postgres_user::remove_user_from_project_in_tx(tx,conduit_admin_graphql::user::RemoveUserFromProjectInput{project_id:project.to_string().into(),user_id:v.user_id.ok_or("user_id is required")?.to_string().into()}).await.map_err(|_|"membership removal rejected")?;vec![project.to_string()]},
            Command::SpendLimit(v)=>{crate::wiring_postgres_billing::set_member_limit_in_tx(tx,v.project_id,v.user_id,v.limit).await.map_err(|_|"member budget configuration failed")?;vec![v.project_id.to_string()]},
            Command::Setting(v)=>{crate::wiring_governance_settings::apply(tx,actor,&v.key,&v.value).await?;vec![v.key]},
            Command::GroupCreate(v)=>vec![crate::wiring_postgres_simple_group::create_simple_group_in_tx(tx,Some(actor),v).await.map_err(|_|"group creation failed")?],
            Command::GroupUpdate(v)=>vec![crate::wiring_postgres_simple_group::update_simple_group_in_tx(tx,Some(actor),v).await.map_err(|_|"group update failed")?],
            Command::GroupAssign(v)=>vec![crate::wiring_postgres_simple_group::assign_simple_group_users_in_tx(tx,Some(actor),v).await.map_err(|_|"group membership failed")?],
            Command::GroupModels(v)=>vec![crate::wiring_postgres_simple_group::update_simple_group_models_in_tx(tx,v).await.map_err(|_|"group models failed")?],
            Command::GroupPrice(v)=>vec![crate::wiring_postgres_simple_group::update_simple_group_price_in_tx(tx,Some(actor),v).await.map_err(|_|"group price failed")?],
            Command::GroupDelete(_)=>{let mut ids=Vec::new();for target in &targets {ids.push(crate::wiring_postgres_simple_group::delete_simple_group_in_tx(tx,Some(actor),target).await.map_err(|_|"group removal failed")?);}ids},
            Command::Credit(v)=>vec![crate::wiring_postgres_billing::grant_project_credit_in_tx(tx,v).await.map_err(|_|"Project credit grant failed")?.to_string()],
            Command::PlanCreate(v)=>vec![crate::wiring_postgres_billing::create_subscription_plan_in_tx(tx,v).await.map_err(|_|"subscription plan creation failed")?.to_string()],
            Command::PlanUpdate(v)=>vec![crate::wiring_postgres_billing::update_subscription_plan_in_tx(tx,v).await.map_err(|_|"subscription plan update failed")?.to_string()],
            Command::SubscriptionAssign(v)=>vec![crate::wiring_postgres_billing::assign_user_subscription_in_tx(tx,v).await.map_err(|_|"subscription assignment failed")?.to_string()],
            Command::RedemptionCreate(v)=>{let payload=crate::wiring_postgres_redemption::create_codes_in_tx(tx,conduit_admin_graphql::billing::CreditRedemptionActor{actor_type:"user".into(),actor_id:Some(actor.to_string())},v).await.map_err(|_|"redemption issuance failed")?;let secret=json!({"codes":payload.codes.iter().map(|c|json!({"id":c.id.to_string(),"code":c.code})).collect::<Vec<_>>()});store_secrets(tx,id,secret).await?;vec![payload.batch_id.to_string()]},
            Command::RedemptionRevoke(_)=>{for target in &targets{crate::wiring_postgres_redemption::revoke_code_in_tx(tx,conduit_admin_graphql::billing::CreditRedemptionActor{actor_type:"user".into(),actor_id:Some(actor.to_string())},target).await.map_err(|_|"redemption revocation failed")?;}targets.clone()},
            Command::SubscriptionRefresh(_)|Command::SubscriptionPause(_)|Command::SubscriptionResume(_)|Command::SubscriptionRenew(_)|Command::SubscriptionAutoRenew(_)=>{let auto=if let Command::SubscriptionAutoRenew(v)=command{v.auto_renew}else{None};let verb=action.strip_prefix("subscription.").ok_or("invalid subscription action")?;for target in &targets {crate::wiring_postgres_billing::subscription_action_in_tx(tx,verb,target.parse().map_err(|_|"invalid subscription target")?,auto).await.map_err(|_|"subscription action failed")?;}targets.clone()},
            Command::SubscriptionCancel(_)=>{let mut ids=Vec::new();for target in &targets{ids.push(crate::wiring_postgres_billing::cancel_user_subscription_in_tx(tx,target).await.map_err(|_|"subscription cancellation failed")?.to_string());}ids},
        };
        for id in ids {
            result.push(json!({"type":registration(&action)?.0,"id":id,"itemKey":"command"}));
        }
    }
    let current_policy = policy::lock_policy(tx).await?;
    policy::ensure_policy_viable(tx, &current_policy).await?;
    Ok(result)
}
fn validate_scopes(scopes: &[String], project: bool) -> Result<(), String> {
    if scopes.iter().any(|s| {
        s != "*" && !conduit_auth::scopes::is_known_scope_slug(s)
            || project && !conduit_auth::scopes::supports_project_role(s)
    }) {
        Err("invalid role scope".into())
    } else {
        Ok(())
    }
}
fn validate_setting(key: &str, value: &Value) -> Result<(), String> {
    crate::wiring_governance_settings::normalize(key, value).map(|_| ())
}

async fn write_fixed_key(
    tx: &mut Transaction<'_, Postgres>,
    value: Value,
    targets: &[String],
) -> Result<Vec<String>, String> {
    if targets.is_empty() {
        let member:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_projects up JOIN users u ON u.id=up.user_id WHERE up.user_id=$1 AND up.project_id=$2 AND u.status='activated' AND u.deleted_at=0)").bind(value["user_id"].as_i64()).bind(value["project_id"].as_i64()).fetch_one(&mut **tx).await.map_err(|_|"key member lookup failed")?;
        if !member {
            return Err("FORBIDDEN: key member does not belong to Project".into());
        }
        return write_row(tx, "api_keys", &value, targets).await;
    }
    for target in targets {
        let current: Value = sqlx::query_scalar(
            "SELECT to_jsonb(k) FROM api_keys k WHERE id::text=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(target)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| "key lookup failed")?;
        if current["type"] == "noauth" {
            return Err("noauth key is immutable".into());
        }
        for field in ["type", "user_id", "project_id"] {
            if value.get(field).is_some_and(|v| v != &current[field]) {
                return Err("key type and ownership are immutable".into());
            }
        }
        if current["type"] == "user" && value.get("scopes").is_some_and(|v| v != &current["scopes"])
        {
            return Err("user key scopes are immutable".into());
        }
    }
    write_row(tx, "api_keys", &value, targets).await
}

async fn store_secrets(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    payload: Value,
) -> Result<(), String> {
    let recipient: i64 = sqlx::query_scalar("SELECT created_by FROM change_sets WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| "secret recipient lookup failed")?;
    sqlx::query("INSERT INTO governed_change_secrets(change_set_id,recipient_user_id,private_payload) VALUES($1,$2,$3) ON CONFLICT(change_set_id) DO UPDATE SET private_payload=governed_change_secrets.private_payload||EXCLUDED.private_payload").bind(id).bind(recipient).bind(sqlx::types::Json(payload)).execute(&mut **tx).await.map_err(|_|"secret delivery storage failed")?;
    Ok(())
}
pub(crate) async fn claim_secrets(
    pool: &PgPool,
    actor: i64,
    id: async_graphql::ID,
) -> Result<Value, String> {
    let id = id
        .as_str()
        .parse::<i64>()
        .map_err(|_| "invalid package ID")?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "secret delivery unavailable")?;
    policy::lock_policy(&mut tx).await?;
    authorize_package(&mut tx, id, actor).await?;
    let value:Option<Value>=sqlx::query_scalar("SELECT private_payload FROM governed_change_secrets WHERE change_set_id=$1 AND recipient_user_id=$2 AND claimed_at IS NULL AND EXISTS(SELECT 1 FROM change_sets WHERE id=$1 AND status='applied') FOR UPDATE").bind(id).bind(actor).fetch_optional(&mut *tx).await.map_err(|_|"secret delivery failed")?;
    let value = value.ok_or("secret delivery is unavailable or already claimed")?;
    sqlx::query("UPDATE governed_change_secrets SET claimed_at=now(),private_payload='{}'::jsonb WHERE change_set_id=$1").bind(id).execute(&mut *tx).await.map_err(|_|"secret delivery failed")?;
    tx.commit().await.map_err(|_| "secret delivery failed")?;
    Ok(value)
}

async fn write_fixed_value(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    value: Value,
    targets: &[String],
) -> Result<Vec<String>, String> {
    write_row(tx, table, &value, targets).await
}
async fn soft_delete_only(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    targets: &[String],
) -> Result<Vec<String>, String> {
    for target in targets {
        sqlx::query(&format!("UPDATE {table} SET deleted_at=EXTRACT(EPOCH FROM now())::bigint,updated_at=now() WHERE id::text=$1 AND deleted_at=0")).bind(target).execute(&mut **tx).await.map_err(|_|"domain removal failed")?;
    }
    Ok(targets.to_vec())
}
async fn apply_channel_keys(
    tx: &mut Transaction<'_, Postgres>,
    target: &str,
    v: &ChannelKeyAction,
) -> Result<(), String> {
    let row=sqlx::query("SELECT credentials,COALESCE(disabled_api_keys,'[]'::jsonb) AS disabled FROM channels WHERE id::text=$1 AND deleted_at=0 FOR UPDATE").bind(target).fetch_one(&mut **tx).await.map_err(|_|"channel key lookup failed")?;
    let mut credentials: conduit_core::objects::channel_settings::ChannelCredentials =
        decode(row.get("credentials"))?;
    let mut disabled: Vec<conduit_core::objects::channel_settings::DisabledAPIKey> =
        decode(row.get("disabled"))?;
    match v.operation {
        ChannelKeyOperation::Disable => {
            for key in &v.keys {
                if credentials
                    .get_all_api_keys()
                    .unwrap_or_default()
                    .contains(key)
                    && !disabled.iter().any(|d| d.key == *key)
                {
                    disabled.push(conduit_core::objects::channel_settings::DisabledAPIKey {
                        key: key.clone(),
                        disabled_at: chrono::Utc::now(),
                        error_code: 0,
                        reason: "Manually disabled by approved change".into(),
                    });
                }
            }
        }
        ChannelKeyOperation::Enable => disabled.retain(|d| !v.keys.contains(&d.key)),
        ChannelKeyOperation::EnableAll => disabled.clear(),
        ChannelKeyOperation::DeleteDisabled => {
            if credentials.is_oauth() {
                return Err("cannot remove OAuth channel keys".into());
            }
            let keys = disabled
                .iter()
                .filter(|d| v.keys.contains(&d.key))
                .map(|d| d.key.clone())
                .collect::<Vec<_>>();
            credentials.api_keys.retain(|key| !keys.contains(key));
            if keys.contains(&credentials.api_key) {
                credentials.api_key.clear();
            }
            if credentials.get_all_api_keys().is_none() {
                return Err("ONE_KEY_PRESERVED: at least one credential must remain".into());
            }
            disabled.retain(|d| !keys.contains(&d.key));
        }
    }
    let exhausted = credentials.get_enabled_api_keys(&disabled).is_none();
    sqlx::query("UPDATE channels SET credentials=$1,disabled_api_keys=$2,status=CASE WHEN $3 THEN 'disabled' ELSE status END,updated_at=now() WHERE id::text=$4").bind(sqlx::types::Json(credentials)).bind(sqlx::types::Json(disabled)).bind(exhausted).bind(target).execute(&mut **tx).await.map_err(|_|"channel key application failed")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn channel_bulk_input(
    channel_type: String,
    name: String,
    base_url: Option<String>,
    key: String,
    supported_models: Vec<String>,
    default_test_model: String,
    settings: Option<conduit_admin_graphql::channel::ChannelSettingsInput>,
    policies: Option<conduit_admin_graphql::channel::ChannelPoliciesInput>,
    tags: Vec<String>,
    auto: Option<bool>,
    weight: Option<i64>,
    remark: Option<String>,
) -> conduit_db::repo::channel_repo::CreateChannelInput {
    conduit_db::repo::channel_repo::CreateChannelInput {
        id: String::new(),
        channel_type,
        name,
        base_url,
        website_url: None,
        quota_currency: Some("USD".into()),
        actual_quota_used: None,
        quota_remaining: None,
        credentials: json!({"apiKeys":[key]}),
        supported_models,
        manual_models: vec![],
        default_test_model,
        auto_sync_supported_models: auto.unwrap_or(false),
        auto_sync_model_pattern: String::new(),
        tags,
        settings: settings.map(crate::wiring_channel_crud::settings_input_to_json),
        policies: policies.map(crate::wiring_channel_crud::policies_input_to_json),
        endpoints: vec![],
        remark,
        ordering_weight: weight.unwrap_or(0),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

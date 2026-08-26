//! RUST-P12-001 S07 (Pauli-the-14th) — Prompt + PromptProtection GraphQL slice.
//!
//! Bounded scope: the `prompts` and `promptProtectionRules` connection queries
//! plus every Prompt- and PromptProtection-rule-domain mutation declared in
//! `conduit/internal/server/gql/prompt.graphql` (lines 52-60) and
//! `prompt_protection_rule.graphql` (lines 37-46). All shapes are copied
//! field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Prompt implements Node` (snapshot line 4590) — scalar/self-domain
//!     fields only; the cross-domain `projects(...)` edge field is pending.
//!   - `type PromptConnection` / `type PromptEdge` (lines 4658 / 4675).
//!   - `type PromptSettings` and friends (lines 9219-9268): `PromptAction`,
//!     `PromptActivationCondition`, `PromptActivationConditionComposite`, plus
//!     the matching `*Input` types.
//!   - `enum PromptActionType` (line 9219), `enum PromptActivationConditionType`
//!     (line 9224), `enum PromptStatus` (line 4887).
//!   - `input CreatePromptInput` (lines 3058-3085, ent-generated).
//!   - `input UpdatePromptInput` (lines 7514-7543, ent-generated).
//!   - `input PromptOrder` / `enum PromptOrderField` (lines 4688 / 4701).
//!   - `input PromptWhereInput` (lines 4895-5030, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `hasProjects` + `hasProjectsWith`.
//!
//!   - `type PromptProtectionRule implements Node` (snapshot line 4706).
//!   - `type PromptProtectionRuleConnection` / `PromptProtectionRuleEdge`
//!     (lines 4731 / 4748).
//!   - `type PromptProtectionSettings` (line 9294),
//!     `type PromptProtectionRulePreviewResult` (line 9300),
//!     `input PromptProtectionSettingsInput` (line 9305),
//!     `input PromptProtectionRulePreviewInput` (line 9311).
//!   - `enum PromptProtectionAction` (line 9281), `enum PromptProtectionScope`
//!     (line 9286), `enum PromptProtectionRuleStatus` (line 4782).
//!   - `input CreatePromptProtectionRuleInput` (lines 3090-3107, ent-generated).
//!   - `input UpdatePromptProtectionRuleInput` (lines 7548-7566, ent-generated).
//!   - `input PromptProtectionRuleOrder` / `enum PromptProtectionRuleOrderField`
//!     (lines 4761 / 4774).
//!   - `input PromptProtectionRuleWhereInput` (lines 4791-4883, ent-generated).
//!
//! Go reference implementations:
//!   - Query.prompts              — `internal/server/gql/ent.resolvers.go:410`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultPromptOrder` = ID before
//!     delegating to ent `Paginate`).
//!   - Query.promptProtectionRules — `internal/server/gql/ent.resolvers.go:425`
//!     (same pattern, lines 427-429).
//!   - Mutation.createPrompt et al. — `prompt.resolvers.go:17-77`, delegating
//!     to `biz.PromptService` (`biz/prompt.go:126` create / `:171` update /
//!     `:241` delete / `:266` status / `:302` bulk delete / `:323` bulk enable /
//!     `:345` bulk disable). Service validates prompt settings, performs a
//!     duplicate-name probe within the same project (xerrors.DuplicateNameError
//!     "prompt"), and applies the partial-merge field set used by the Go
//!     `UpdateOne` builder (SetNillable{Description, Name, Role, Content,
//!     Order, Status} + conditional SetSettings).
//!   - Mutation.createPromptProtectionRule et al. —
//!     `prompt_protection_rule.resolvers.go:18-91`, delegating to
//!     `biz.PromptProtectionRuleService` (`biz/prompt_protection_rule.go:221`
//!     create / `:253` update / `:283` delete / `:293` status /
//!     `:306` bulk delete / `:322` bulk disable / `:339` bulk enable). Service
//!     validates pattern+settings, performs a duplicate-name probe
//!     (xerrors.DuplicateNameError "prompt protection rule"), and the UpdateRule
//!     path resolves the effective pattern/settings BEFORE re-validating
//!     (lo.FromPtrOr + current settings fallback).
//!   - Mutation.previewPromptProtectionRule — `prompt_protection_rule.resolvers.go:73-91`,
//!     delegating to `biz.PromptProtectionRuleService.Preview`
//!     (`biz/prompt_protection_preview.go:21`): validate settings, compile
//!     regex (regexp2), match against test text; if action=mask → replace; if
//!     action=reject → result is the literal "reject".
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//!   - `Prompt.projects(...)`: cross-domain edge field into Project domain.
//!   - `PromptWhereInput.hasProjectsWith: [ProjectWhereInput!]` (references
//!     another entity's WhereInput — left pending to keep the prompt slice
//!     self-contained; matches the channel/role convention).
//!   - The single-object `prompt(id: ID!)` lookup goes through the global
//!     `node(id: ID!)` Relay query (separate slice).

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, TimeScalar};

// ===========================================================================
// Enums
// ===========================================================================

/// `enum PromptStatus { enabled disabled }` — snapshot line 4887, bound to Go
/// `ent/prompt.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum PromptStatus {
    #[graphql(name = "enabled")]
    Enabled,
    #[graphql(name = "disabled")]
    Disabled,
}

/// `enum PromptActionType { prepend append }` — snapshot line 9219, bound to Go
/// `objects.PromptActionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptActionType {
    #[graphql(name = "prepend")]
    Prepend,
    #[graphql(name = "append")]
    Append,
}

/// `enum PromptActivationConditionType { model_id model_pattern api_key }` —
/// snapshot line 9224, bound to Go `objects.PromptActivationConditionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptActivationConditionType {
    #[graphql(name = "model_id")]
    ModelId,
    #[graphql(name = "model_pattern")]
    ModelPattern,
    #[graphql(name = "api_key")]
    ApiKey,
}

/// `enum PromptOrderField { CREATED_AT UPDATED_AT ORDER }` — snapshot lines
/// 4701-4704.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
    #[graphql(name = "ORDER")]
    Order,
}

/// `enum PromptProtectionAction { mask reject }` — snapshot line 9281, bound to
/// Go `objects.PromptProtectionAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptProtectionAction {
    #[graphql(name = "mask")]
    Mask,
    #[graphql(name = "reject")]
    Reject,
}

/// `enum PromptProtectionScope { system developer user assistant tool }` —
/// snapshot line 9286, bound to Go `objects.PromptProtectionScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptProtectionScope {
    #[graphql(name = "system")]
    System,
    #[graphql(name = "developer")]
    Developer,
    #[graphql(name = "user")]
    User,
    #[graphql(name = "assistant")]
    Assistant,
    #[graphql(name = "tool")]
    Tool,
}

/// `enum PromptProtectionRuleStatus { enabled disabled archived }` — snapshot
/// line 4782, bound to Go `ent/promptprotectionrule.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum PromptProtectionRuleStatus {
    #[graphql(name = "enabled")]
    Enabled,
    #[graphql(name = "disabled")]
    Disabled,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum PromptProtectionRuleOrderField { CREATED_AT UPDATED_AT NAME }` —
/// snapshot lines 4774-4777.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptProtectionRuleOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
    #[graphql(name = "NAME")]
    Name,
}

// ===========================================================================
// Output object types
// ===========================================================================

/// `type PromptAction { type: PromptActionType! }` — snapshot line 9230.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "PromptAction")]
pub struct PromptAction {
    // `type` is a Rust keyword; the GraphQL field name is pinned explicitly.
    #[graphql(name = "type")]
    pub action_type: PromptActionType,
}

/// `type PromptActivationCondition` — snapshot lines 9234-9239.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PromptActivationCondition {
    #[graphql(name = "type")]
    pub condition_type: PromptActivationConditionType,
    pub model_id: Option<String>,
    pub model_pattern: Option<String>,
    pub api_key_id: Option<i64>,
}

/// `type PromptActivationConditionComposite` — snapshot line 9241.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PromptActivationConditionComposite {
    pub conditions: Option<Vec<PromptActivationCondition>>,
}

/// `type PromptSettings` — snapshot lines 9245-9248.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PromptSettings {
    pub action: PromptAction,
    pub conditions: Option<Vec<PromptActivationConditionComposite>>,
}

/// `type Prompt implements Node` — snapshot lines 4590-4654, scalar and
/// self-domain fields only. Cross-domain edge field `projects(...)` is pending
/// (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct Prompt {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    // All-caps acronym tag: default camelCase would emit `projectId`.
    #[graphql(name = "projectID")]
    pub project_id: i64,
    pub name: String,
    pub description: String,
    pub role: String,
    pub content: String,
    pub status: PromptStatus,
    pub order: i64,
    pub settings: PromptSettings,
}

/// `type PromptEdge { node: Prompt cursor: Cursor! }` — snapshot line 4675.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct PromptEdge {
    pub node: Option<Prompt>,
    pub cursor: CursorScalar,
}

/// `type PromptConnection` — snapshot line 4658.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct PromptConnection {
    pub edges: Option<Vec<Option<PromptEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `type PromptProtectionSettings` — snapshot lines 9294-9298.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PromptProtectionSettings {
    pub action: PromptProtectionAction,
    pub replacement: Option<String>,
    pub scopes: Vec<PromptProtectionScope>,
}

/// `type PromptProtectionRulePreviewResult` — snapshot lines 9300-9303.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PromptProtectionRulePreviewResult {
    pub result: String,
    pub has_match: bool,
}

/// `type PromptProtectionRule implements Node` — snapshot lines 4706-4727.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct PromptProtectionRule {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub name: String,
    pub description: String,
    pub pattern: String,
    pub status: PromptProtectionRuleStatus,
    pub settings: PromptProtectionSettings,
}

/// `type PromptProtectionRuleEdge` — snapshot line 4748.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct PromptProtectionRuleEdge {
    pub node: Option<PromptProtectionRule>,
    pub cursor: CursorScalar,
}

/// `type PromptProtectionRuleConnection` — snapshot line 4731.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct PromptProtectionRuleConnection {
    pub edges: Option<Vec<Option<PromptProtectionRuleEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ===========================================================================
// Input object types
// ===========================================================================

/// `input PromptActionInput` — snapshot line 9250.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptActionInput {
    #[graphql(name = "type")]
    pub action_type: PromptActionType,
}

/// `input PromptActivationConditionInput` — snapshot lines 9254-9259.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptActivationConditionInput {
    #[graphql(name = "type")]
    pub condition_type: PromptActivationConditionType,
    pub model_id: Option<String>,
    pub model_pattern: Option<String>,
    pub api_key_id: Option<i64>,
}

/// `input PromptActivationConditionCompositeInput` — snapshot line 9261.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptActivationConditionCompositeInput {
    pub conditions: Option<Vec<PromptActivationConditionInput>>,
}

/// `input PromptSettingsInput` — snapshot lines 9265-9268.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptSettingsInput {
    pub action: PromptActionInput,
    pub conditions: Option<Vec<PromptActivationConditionCompositeInput>>,
}

/// `input CreatePromptInput` — snapshot lines 3058-3085 (ent-generated).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreatePromptInput {
    pub name: String,
    pub description: Option<String>,
    pub role: String,
    pub content: String,
    pub status: Option<PromptStatus>,
    pub order: Option<i64>,
    pub settings: PromptSettingsInput,
    // All-caps acronym tag: default camelCase would emit `projectIds`.
    #[graphql(name = "projectIDs")]
    pub project_ids: Option<Vec<ID>>,
}

/// `input UpdatePromptInput` — snapshot lines 7514-7543 (ent-generated).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdatePromptInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub content: Option<String>,
    pub status: Option<PromptStatus>,
    pub order: Option<i64>,
    pub settings: Option<PromptSettingsInput>,
    #[graphql(name = "addProjectIDs")]
    pub add_project_ids: Option<Vec<ID>>,
    #[graphql(name = "removeProjectIDs")]
    pub remove_project_ids: Option<Vec<ID>>,
    pub clear_projects: Option<bool>,
}

/// `input PromptOrder { direction: OrderDirection! = ASC field:
/// PromptOrderField! }` — snapshot lines 4688-4697.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: PromptOrderField,
}

/// `input PromptProtectionSettingsInput` — snapshot lines 9305-9309.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptProtectionSettingsInput {
    pub action: PromptProtectionAction,
    pub replacement: Option<String>,
    pub scopes: Option<Vec<PromptProtectionScope>>,
}

/// `input PromptProtectionRulePreviewInput` — snapshot lines 9311-9315.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptProtectionRulePreviewInput {
    pub pattern: String,
    pub test_text: String,
    pub settings: PromptProtectionSettingsInput,
}

/// `input CreatePromptProtectionRuleInput` — snapshot lines 3090-3107
/// (ent-generated).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreatePromptProtectionRuleInput {
    pub name: String,
    pub description: Option<String>,
    pub pattern: String,
    pub settings: PromptProtectionSettingsInput,
}

/// `input UpdatePromptProtectionRuleInput` — snapshot lines 7548-7566
/// (ent-generated).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdatePromptProtectionRuleInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub pattern: Option<String>,
    pub status: Option<PromptProtectionRuleStatus>,
    pub settings: Option<PromptProtectionSettingsInput>,
}

/// `input PromptProtectionRuleOrder` — snapshot lines 4761-4770.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct PromptProtectionRuleOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: PromptProtectionRuleOrderField,
}

/// `input PromptWhereInput` — snapshot lines 4895-5030 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family, and `hasProjects` (existence). `hasProjectsWith` is
/// listed as pending (module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct PromptWhereInput {
    pub not: Option<Box<PromptWhereInput>>,
    pub and: Option<Vec<PromptWhereInput>>,
    pub or: Option<Vec<PromptWhereInput>>,
    // id field predicates
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    pub id_in: Option<Vec<ID>>,
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    pub created_at_in: Option<Vec<TimeScalar>>,
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    pub updated_at_in: Option<Vec<TimeScalar>>,
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // project_id field predicates (acronym rename: projectID*)
    #[graphql(name = "projectID")]
    pub project_id: Option<i64>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<i64>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<i64>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<i64>>,
    #[graphql(name = "projectIDGT")]
    pub project_id_gt: Option<i64>,
    #[graphql(name = "projectIDGTE")]
    pub project_id_gte: Option<i64>,
    #[graphql(name = "projectIDLT")]
    pub project_id_lt: Option<i64>,
    #[graphql(name = "projectIDLTE")]
    pub project_id_lte: Option<i64>,
    // name field predicates
    pub name: Option<String>,
    #[graphql(name = "nameNEQ")]
    pub name_neq: Option<String>,
    pub name_in: Option<Vec<String>>,
    pub name_not_in: Option<Vec<String>>,
    #[graphql(name = "nameGT")]
    pub name_gt: Option<String>,
    #[graphql(name = "nameGTE")]
    pub name_gte: Option<String>,
    #[graphql(name = "nameLT")]
    pub name_lt: Option<String>,
    #[graphql(name = "nameLTE")]
    pub name_lte: Option<String>,
    pub name_contains: Option<String>,
    pub name_has_prefix: Option<String>,
    pub name_has_suffix: Option<String>,
    pub name_equal_fold: Option<String>,
    pub name_contains_fold: Option<String>,
    // description field predicates
    pub description: Option<String>,
    #[graphql(name = "descriptionNEQ")]
    pub description_neq: Option<String>,
    pub description_in: Option<Vec<String>>,
    pub description_not_in: Option<Vec<String>>,
    #[graphql(name = "descriptionGT")]
    pub description_gt: Option<String>,
    #[graphql(name = "descriptionGTE")]
    pub description_gte: Option<String>,
    #[graphql(name = "descriptionLT")]
    pub description_lt: Option<String>,
    #[graphql(name = "descriptionLTE")]
    pub description_lte: Option<String>,
    pub description_contains: Option<String>,
    pub description_has_prefix: Option<String>,
    pub description_has_suffix: Option<String>,
    pub description_equal_fold: Option<String>,
    pub description_contains_fold: Option<String>,
    // role field predicates
    pub role: Option<String>,
    #[graphql(name = "roleNEQ")]
    pub role_neq: Option<String>,
    pub role_in: Option<Vec<String>>,
    pub role_not_in: Option<Vec<String>>,
    #[graphql(name = "roleGT")]
    pub role_gt: Option<String>,
    #[graphql(name = "roleGTE")]
    pub role_gte: Option<String>,
    #[graphql(name = "roleLT")]
    pub role_lt: Option<String>,
    #[graphql(name = "roleLTE")]
    pub role_lte: Option<String>,
    pub role_contains: Option<String>,
    pub role_has_prefix: Option<String>,
    pub role_has_suffix: Option<String>,
    pub role_equal_fold: Option<String>,
    pub role_contains_fold: Option<String>,
    // content field predicates
    pub content: Option<String>,
    #[graphql(name = "contentNEQ")]
    pub content_neq: Option<String>,
    pub content_in: Option<Vec<String>>,
    pub content_not_in: Option<Vec<String>>,
    #[graphql(name = "contentGT")]
    pub content_gt: Option<String>,
    #[graphql(name = "contentGTE")]
    pub content_gte: Option<String>,
    #[graphql(name = "contentLT")]
    pub content_lt: Option<String>,
    #[graphql(name = "contentLTE")]
    pub content_lte: Option<String>,
    pub content_contains: Option<String>,
    pub content_has_prefix: Option<String>,
    pub content_has_suffix: Option<String>,
    pub content_equal_fold: Option<String>,
    pub content_contains_fold: Option<String>,
    // status field predicates
    pub status: Option<PromptStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<PromptStatus>,
    pub status_in: Option<Vec<PromptStatus>>,
    pub status_not_in: Option<Vec<PromptStatus>>,
    // order field predicates
    pub order: Option<i64>,
    #[graphql(name = "orderNEQ")]
    pub order_neq: Option<i64>,
    pub order_in: Option<Vec<i64>>,
    pub order_not_in: Option<Vec<i64>>,
    #[graphql(name = "orderGT")]
    pub order_gt: Option<i64>,
    #[graphql(name = "orderGTE")]
    pub order_gte: Option<i64>,
    #[graphql(name = "orderLT")]
    pub order_lt: Option<i64>,
    #[graphql(name = "orderLTE")]
    pub order_lte: Option<i64>,
    // projects edge predicates
    pub has_projects: Option<bool>,
}

/// `input PromptProtectionRuleWhereInput` — snapshot lines 4791-4883
/// (ent-generated predicate grammar).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct PromptProtectionRuleWhereInput {
    pub not: Option<Box<PromptProtectionRuleWhereInput>>,
    pub and: Option<Vec<PromptProtectionRuleWhereInput>>,
    pub or: Option<Vec<PromptProtectionRuleWhereInput>>,
    // id field predicates
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    pub id_in: Option<Vec<ID>>,
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    pub created_at_in: Option<Vec<TimeScalar>>,
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    pub updated_at_in: Option<Vec<TimeScalar>>,
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // name field predicates
    pub name: Option<String>,
    #[graphql(name = "nameNEQ")]
    pub name_neq: Option<String>,
    pub name_in: Option<Vec<String>>,
    pub name_not_in: Option<Vec<String>>,
    #[graphql(name = "nameGT")]
    pub name_gt: Option<String>,
    #[graphql(name = "nameGTE")]
    pub name_gte: Option<String>,
    #[graphql(name = "nameLT")]
    pub name_lt: Option<String>,
    #[graphql(name = "nameLTE")]
    pub name_lte: Option<String>,
    pub name_contains: Option<String>,
    pub name_has_prefix: Option<String>,
    pub name_has_suffix: Option<String>,
    pub name_equal_fold: Option<String>,
    pub name_contains_fold: Option<String>,
    // description field predicates
    pub description: Option<String>,
    #[graphql(name = "descriptionNEQ")]
    pub description_neq: Option<String>,
    pub description_in: Option<Vec<String>>,
    pub description_not_in: Option<Vec<String>>,
    #[graphql(name = "descriptionGT")]
    pub description_gt: Option<String>,
    #[graphql(name = "descriptionGTE")]
    pub description_gte: Option<String>,
    #[graphql(name = "descriptionLT")]
    pub description_lt: Option<String>,
    #[graphql(name = "descriptionLTE")]
    pub description_lte: Option<String>,
    pub description_contains: Option<String>,
    pub description_has_prefix: Option<String>,
    pub description_has_suffix: Option<String>,
    pub description_equal_fold: Option<String>,
    pub description_contains_fold: Option<String>,
    // pattern field predicates
    pub pattern: Option<String>,
    #[graphql(name = "patternNEQ")]
    pub pattern_neq: Option<String>,
    pub pattern_in: Option<Vec<String>>,
    pub pattern_not_in: Option<Vec<String>>,
    #[graphql(name = "patternGT")]
    pub pattern_gt: Option<String>,
    #[graphql(name = "patternGTE")]
    pub pattern_gte: Option<String>,
    #[graphql(name = "patternLT")]
    pub pattern_lt: Option<String>,
    #[graphql(name = "patternLTE")]
    pub pattern_lte: Option<String>,
    pub pattern_contains: Option<String>,
    pub pattern_has_prefix: Option<String>,
    pub pattern_has_suffix: Option<String>,
    pub pattern_equal_fold: Option<String>,
    pub pattern_contains_fold: Option<String>,
    // status field predicates
    pub status: Option<PromptProtectionRuleStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<PromptProtectionRuleStatus>,
    pub status_in: Option<Vec<PromptProtectionRuleStatus>>,
    pub status_not_in: Option<Vec<PromptProtectionRuleStatus>>,
}

// ===========================================================================
// Input → object conversions
// ===========================================================================

impl From<PromptActionInput> for PromptAction {
    fn from(input: PromptActionInput) -> Self {
        Self {
            action_type: input.action_type,
        }
    }
}

impl From<PromptActivationConditionInput> for PromptActivationCondition {
    fn from(input: PromptActivationConditionInput) -> Self {
        Self {
            condition_type: input.condition_type,
            model_id: input.model_id,
            model_pattern: input.model_pattern,
            api_key_id: input.api_key_id,
        }
    }
}

impl From<PromptActivationConditionCompositeInput> for PromptActivationConditionComposite {
    fn from(input: PromptActivationConditionCompositeInput) -> Self {
        Self {
            conditions: input
                .conditions
                .map(|v| v.into_iter().map(PromptActivationCondition::from).collect()),
        }
    }
}

impl From<PromptSettingsInput> for PromptSettings {
    fn from(input: PromptSettingsInput) -> Self {
        Self {
            action: PromptAction::from(input.action),
            conditions: input.conditions.map(|v| {
                v.into_iter()
                    .map(PromptActivationConditionComposite::from)
                    .collect()
            }),
        }
    }
}

impl From<PromptProtectionSettingsInput> for PromptProtectionSettings {
    fn from(input: PromptProtectionSettingsInput) -> Self {
        // Output `scopes` is non-null list; Go's SetSettings path persists
        // whatever the input carried, but the read-side force-resolver
        // zero-fills nil scopes to an empty slice for legacy rows.
        Self {
            action: input.action,
            replacement: input.replacement,
            scopes: input.scopes.unwrap_or_default(),
        }
    }
}

// ===========================================================================
// Ordering resolution
// ===========================================================================

/// Internal ordering terms the service layer receives. `Id` is NOT part of the
/// GraphQL `PromptOrderField` enum — it is ent's `DefaultPromptOrder`
/// (order by primary key), which the Go resolver substitutes when the client
/// asks for `CREATED_AT` (ent.resolvers.go:413-415).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOrderTerm {
    Id,
    UpdatedAt,
    Order,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptOrderSelection {
    pub direction: OrderDirection,
    pub term: PromptOrderTerm,
}

/// Mirrors Go `Query.prompts` (ent.resolvers.go:413-415): a `CREATED_AT`
/// request is remapped to ent's default ID order with direction preserved;
/// `UPDATED_AT` and `ORDER` map one-to-one.
pub fn resolve_prompt_order(order_by: Option<PromptOrder>) -> Option<PromptOrderSelection> {
    order_by.map(|order| PromptOrderSelection {
        direction: order.direction,
        term: match order.field {
            PromptOrderField::CreatedAt => PromptOrderTerm::Id,
            PromptOrderField::UpdatedAt => PromptOrderTerm::UpdatedAt,
            PromptOrderField::Order => PromptOrderTerm::Order,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptProtectionRuleOrderTerm {
    Id,
    UpdatedAt,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptProtectionRuleOrderSelection {
    pub direction: OrderDirection,
    pub term: PromptProtectionRuleOrderTerm,
}

/// Mirrors Go `Query.promptProtectionRules` (ent.resolvers.go:427-429):
/// `CREATED_AT` is remapped to ent's default ID order with direction preserved.
pub fn resolve_prompt_protection_rule_order(
    order_by: Option<PromptProtectionRuleOrder>,
) -> Option<PromptProtectionRuleOrderSelection> {
    order_by.map(|order| PromptProtectionRuleOrderSelection {
        direction: order.direction,
        term: match order.field {
            PromptProtectionRuleOrderField::CreatedAt => PromptProtectionRuleOrderTerm::Id,
            PromptProtectionRuleOrderField::UpdatedAt => PromptProtectionRuleOrderTerm::UpdatedAt,
            PromptProtectionRuleOrderField::Name => PromptProtectionRuleOrderTerm::Name,
        },
    })
}

// ===========================================================================
// Service traits (host-injected, mirroring the Go resolver's dependencies)
// ===========================================================================

/// Error surface for the prompt / prompt-protection services. Messages mirror
/// the Go error strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("prompt", name)` /
///     `xerrors.DuplicateNameError("prompt protection rule", name)`
///     (`internal/pkg/xerrors/graphql.go:104`): `"%s name '%s' already exists"`.
///   - validation errors — `biz/prompt.go:88-124` (model_id/model_pattern/
///     api_key_id consistency) and `biz/prompt_protection_rule.go:92-123`
///     (regex compile + mask-replacement-required + scope-validation).
///   - not-found — `biz/prompt.go:223` ("prompt not found or not in project").
///   - wrapped create/update/delete failures — `fmt.Errorf("failed to ...: %w")`
///     prefixes.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PromptServiceError {
    #[error("prompt service is not available")]
    ServiceUnavailable,
    #[error("prompt name '{0}' already exists")]
    DuplicatePromptName(String),
    #[error("prompt protection rule name '{0}' already exists")]
    DuplicateRuleName(String),
    #[error("invalid prompt settings: {0}")]
    InvalidPromptSettings(String),
    #[error("invalid prompt protection settings: {0}")]
    InvalidRuleSettings(String),
    #[error("prompt not found or not in project")]
    PromptNotFound,
    #[error("failed to query prompt protection rule: {0}")]
    RuleQuery(String),
    #[error("failed to create prompt: {0}")]
    CreatePrompt(String),
    #[error("failed to update prompt: {0}")]
    UpdatePrompt(String),
    #[error("failed to delete prompt: {0}")]
    DeletePrompt(String),
    #[error("failed to create prompt protection rule: {0}")]
    CreateRule(String),
    #[error("failed to update prompt protection rule: {0}")]
    UpdateRule(String),
    #[error("failed to delete prompt protection rule: {0}")]
    DeleteRule(String),
    #[error("failed to query prompts: {0}")]
    PromptQuery(String),
    #[error("failed to query prompt protection rules: {0}")]
    RuleQueryList(String),
}

/// Arguments for the `prompts` connection query.
#[derive(Debug, Clone, Default)]
pub struct PromptConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<PromptOrderSelection>,
    pub where_filter: Option<PromptWhereInput>,
}

/// Arguments for the `promptProtectionRules` connection query.
#[derive(Debug, Clone, Default)]
pub struct PromptProtectionRuleConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<PromptProtectionRuleOrderSelection>,
    pub where_filter: Option<PromptProtectionRuleWhereInput>,
}

/// Backs `Query.prompts` (Go ent.resolvers.go:410).
#[async_trait::async_trait]
pub trait PromptQueryServices: Send + Sync {
    async fn prompts(
        &self,
        args: PromptConnectionArgs,
    ) -> Result<PromptConnection, PromptServiceError>;
}

/// Backs `Query.promptProtectionRules` (Go ent.resolvers.go:425).
#[async_trait::async_trait]
pub trait PromptProtectionRuleQueryServices: Send + Sync {
    async fn prompt_protection_rules(
        &self,
        args: PromptProtectionRuleConnectionArgs,
    ) -> Result<PromptProtectionRuleConnection, PromptServiceError>;
}

/// Backs the seven Prompt mutations (Go `biz.PromptService`).
#[async_trait::async_trait]
pub trait PromptMutationServices: Send + Sync {
    /// Mirrors `PromptService.CreatePrompt` (biz/prompt.go:126).
    async fn create_prompt(&self, input: CreatePromptInput) -> Result<Prompt, PromptServiceError>;

    /// Mirrors `PromptService.UpdatePrompt` (biz/prompt.go:171).
    async fn update_prompt(
        &self,
        id: &str,
        input: UpdatePromptInput,
    ) -> Result<Prompt, PromptServiceError>;

    /// Mirrors `PromptService.DeletePrompt` (biz/prompt.go:241).
    async fn delete_prompt(&self, id: &str) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptService.UpdatePromptStatus` (biz/prompt.go:266).
    async fn update_prompt_status(
        &self,
        id: &str,
        status: PromptStatus,
    ) -> Result<Prompt, PromptServiceError>;

    /// Mirrors `PromptService.BulkDeletePrompts` (biz/prompt.go:302).
    async fn bulk_delete_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptService.BulkEnablePrompts` (biz/prompt.go:323).
    async fn bulk_enable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptService.BulkDisablePrompts` (biz/prompt.go:345).
    async fn bulk_disable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError>;
}

/// Backs the eight PromptProtectionRule mutations (Go
/// `biz.PromptProtectionRuleService`).
#[async_trait::async_trait]
pub trait PromptProtectionRuleMutationServices: Send + Sync {
    /// Mirrors `PromptProtectionRuleService.CreateRule`
    /// (biz/prompt_protection_rule.go:221).
    async fn create_prompt_protection_rule(
        &self,
        input: CreatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.UpdateRule`
    /// (biz/prompt_protection_rule.go:253).
    async fn update_prompt_protection_rule(
        &self,
        id: &str,
        input: UpdatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.DeleteRule`
    /// (biz/prompt_protection_rule.go:283).
    async fn delete_prompt_protection_rule(&self, id: &str) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.UpdateRuleStatus`
    /// (biz/prompt_protection_rule.go:293).
    async fn update_prompt_protection_rule_status(
        &self,
        id: &str,
        status: PromptProtectionRuleStatus,
    ) -> Result<PromptProtectionRule, PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.BulkDeleteRules`
    /// (biz/prompt_protection_rule.go:306). Empty ids is a no-op.
    async fn bulk_delete_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.BulkDisableRules`
    /// (biz/prompt_protection_rule.go:322). Empty ids is a no-op.
    async fn bulk_disable_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.BulkEnableRules`
    /// (biz/prompt_protection_rule.go:339). Empty ids is a no-op.
    async fn bulk_enable_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError>;

    /// Mirrors `PromptProtectionRuleService.Preview`
    /// (biz/prompt_protection_preview.go:21). Compiles the supplied regex,
    /// matches against `test_text`, applies mask/reject per `settings.action`,
    /// and returns the resulting text plus `has_match`.
    async fn preview_prompt_protection_rule(
        &self,
        input: PromptProtectionRulePreviewInput,
    ) -> Result<PromptProtectionRulePreviewResult, PromptServiceError>;
}

pub(crate) fn prompt_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn PromptQueryServices>, String> {
    match ctx.data::<Arc<dyn PromptQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(PromptServiceError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn prompt_protection_rule_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn PromptProtectionRuleQueryServices>, String> {
    match ctx.data::<Arc<dyn PromptProtectionRuleQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(PromptServiceError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn prompt_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn PromptMutationServices>, String> {
    match ctx.data::<Arc<dyn PromptMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(PromptServiceError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn prompt_protection_rule_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn PromptProtectionRuleMutationServices>, String> {
    match ctx.data::<Arc<dyn PromptProtectionRuleMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(PromptServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::ID;

    use super::*;
    use crate::pagination::connection_from_offset_page;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_settings() -> PromptSettings {
        PromptSettings {
            action: PromptAction {
                action_type: PromptActionType::Prepend,
            },
            conditions: None,
        }
    }

    fn sample_prompt(id: i64, name: &str) -> Prompt {
        Prompt {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            project_id: 1,
            name: name.to_owned(),
            description: String::new(),
            role: "system".to_owned(),
            content: "hello".to_owned(),
            status: PromptStatus::Enabled,
            order: 0,
            settings: sample_settings(),
        }
    }

    fn sample_protection_settings() -> PromptProtectionSettings {
        PromptProtectionSettings {
            action: PromptProtectionAction::Mask,
            replacement: Some("***".to_owned()),
            scopes: vec![PromptProtectionScope::User],
        }
    }

    fn sample_rule(id: i64, name: &str) -> PromptProtectionRule {
        PromptProtectionRule {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            name: name.to_owned(),
            description: String::new(),
            pattern: r"secret-\d+".to_owned(),
            status: PromptProtectionRuleStatus::Enabled,
            settings: sample_protection_settings(),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory service doubles. Mirror the Go biz service call sequences
    // without DB / regex compile / scheduling concerns.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryPromptService {
        prompts: Arc<Mutex<Vec<Prompt>>>,
        captured_query_args: Arc<Mutex<Vec<PromptConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl PromptQueryServices for InMemoryPromptService {
        async fn prompts(
            &self,
            args: PromptConnectionArgs,
        ) -> Result<PromptConnection, PromptServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<Prompt> = lock(&self.prompts).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        PromptOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        PromptOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
                        PromptOrderTerm::Order => a.order.cmp(&b.order),
                    };
                    match selection.direction {
                        OrderDirection::Asc => ordering,
                        OrderDirection::Desc => ordering.reverse(),
                    }
                });
            }

            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
            Ok(PromptConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(PromptEdge {
                                node: Some(edge.node),
                                cursor: CursorScalar(edge.cursor),
                            })
                        })
                        .collect(),
                ),
                page_info: connection.page_info,
                total_count,
            })
        }
    }

    #[async_trait::async_trait]
    impl PromptMutationServices for InMemoryPromptService {
        async fn create_prompt(
            &self,
            input: CreatePromptInput,
        ) -> Result<Prompt, PromptServiceError> {
            let mut guard = lock(&self.prompts);
            // biz/prompt.go:137-149: duplicate-name probe scoped to project.
            if guard.iter().any(|existing| existing.name == input.name) {
                return Err(PromptServiceError::DuplicatePromptName(input.name));
            }

            let id = guard.len() as i64 + 1;
            let created = Prompt {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                project_id: 1,
                name: input.name,
                description: input.description.unwrap_or_default(),
                role: input.role,
                content: input.content,
                // ent default (biz/prompt.go:158): status = enabled when nil.
                status: input.status.unwrap_or(PromptStatus::Enabled),
                order: input.order.unwrap_or(0),
                settings: PromptSettings::from(input.settings),
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_prompt(
            &self,
            id: &str,
            input: UpdatePromptInput,
        ) -> Result<Prompt, PromptServiceError> {
            let mut guard = lock(&self.prompts);
            // biz/prompt.go:184-199: duplicate-name probe excluding self.
            if let Some(name) = &input.name
                && guard
                    .iter()
                    .any(|other| other.name == *name && other.id.as_str() != id)
            {
                return Err(PromptServiceError::DuplicatePromptName(name.clone()));
            }

            let Some(prompt) = guard.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(PromptServiceError::UpdatePrompt(
                    PromptServiceError::PromptNotFound.to_string(),
                ));
            };

            // Field application mirrors Go biz/prompt.go:201-211:
            // SetNillable{Description, Name, Role, Content, Order, Status} +
            // conditional SetSettings (when input.Settings != nil).
            if let Some(v) = input.name {
                prompt.name = v;
            }
            if let Some(v) = input.description {
                prompt.description = v;
            }
            if let Some(v) = input.role {
                prompt.role = v;
            }
            if let Some(v) = input.content {
                prompt.content = v;
            }
            if let Some(v) = input.order {
                prompt.order = v;
            }
            if let Some(v) = input.status {
                prompt.status = v;
            }
            if let Some(v) = input.settings {
                prompt.settings = PromptSettings::from(v);
            }

            Ok(prompt.clone())
        }

        async fn delete_prompt(&self, id: &str) -> Result<(), PromptServiceError> {
            let mut guard = lock(&self.prompts);
            let before = guard.len();
            guard.retain(|p| p.id.as_str() != id);
            if guard.len() == before {
                return Err(PromptServiceError::DeletePrompt(
                    PromptServiceError::PromptNotFound.to_string(),
                ));
            }
            Ok(())
        }

        async fn update_prompt_status(
            &self,
            id: &str,
            status: PromptStatus,
        ) -> Result<Prompt, PromptServiceError> {
            let mut guard = lock(&self.prompts);
            let Some(prompt) = guard.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(PromptServiceError::UpdatePrompt(
                    PromptServiceError::PromptNotFound.to_string(),
                ));
            };
            prompt.status = status;
            Ok(prompt.clone())
        }

        async fn bulk_delete_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
            let mut guard = lock(&self.prompts);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            guard.retain(|p| !id_set.contains(p.id.as_str()));
            Ok(())
        }

        async fn bulk_enable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
            let mut guard = lock(&self.prompts);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            for prompt in guard.iter_mut() {
                if id_set.contains(prompt.id.as_str()) {
                    prompt.status = PromptStatus::Enabled;
                }
            }
            Ok(())
        }

        async fn bulk_disable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
            let mut guard = lock(&self.prompts);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            for prompt in guard.iter_mut() {
                if id_set.contains(prompt.id.as_str()) {
                    prompt.status = PromptStatus::Disabled;
                }
            }
            Ok(())
        }
    }

    #[derive(Default, Clone)]
    struct InMemoryPromptProtectionRuleService {
        rules: Arc<Mutex<Vec<PromptProtectionRule>>>,
        captured_query_args: Arc<Mutex<Vec<PromptProtectionRuleConnectionArgs>>>,
        last_preview: Arc<Mutex<Option<PromptProtectionRulePreviewInput>>>,
    }

    #[async_trait::async_trait]
    impl PromptProtectionRuleQueryServices for InMemoryPromptProtectionRuleService {
        async fn prompt_protection_rules(
            &self,
            args: PromptProtectionRuleConnectionArgs,
        ) -> Result<PromptProtectionRuleConnection, PromptServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<PromptProtectionRule> = lock(&self.rules).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        PromptProtectionRuleOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        PromptProtectionRuleOrderTerm::UpdatedAt => {
                            a.updated_at.0.cmp(&b.updated_at.0)
                        }
                        PromptProtectionRuleOrderTerm::Name => a.name.cmp(&b.name),
                    };
                    match selection.direction {
                        OrderDirection::Asc => ordering,
                        OrderDirection::Desc => ordering.reverse(),
                    }
                });
            }

            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
            Ok(PromptProtectionRuleConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(PromptProtectionRuleEdge {
                                node: Some(edge.node),
                                cursor: CursorScalar(edge.cursor),
                            })
                        })
                        .collect(),
                ),
                page_info: connection.page_info,
                total_count,
            })
        }
    }

    #[async_trait::async_trait]
    impl PromptProtectionRuleMutationServices for InMemoryPromptProtectionRuleService {
        async fn create_prompt_protection_rule(
            &self,
            input: CreatePromptProtectionRuleInput,
        ) -> Result<PromptProtectionRule, PromptServiceError> {
            let mut guard = lock(&self.rules);
            // biz/prompt_protection_rule.go:230-239: duplicate-name probe.
            if guard.iter().any(|existing| existing.name == input.name) {
                return Err(PromptServiceError::DuplicateRuleName(input.name));
            }

            let id = guard.len() as i64 + 1;
            let created = PromptProtectionRule {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                name: input.name,
                description: input.description.unwrap_or_default(),
                pattern: input.pattern,
                status: PromptProtectionRuleStatus::Enabled,
                settings: PromptProtectionSettings::from(input.settings),
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_prompt_protection_rule(
            &self,
            id: &str,
            input: UpdatePromptProtectionRuleInput,
        ) -> Result<PromptProtectionRule, PromptServiceError> {
            let mut guard = lock(&self.rules);
            let Some(rule) = guard.iter_mut().find(|r| r.id.as_str() == id) else {
                return Err(PromptServiceError::UpdateRule("rule not found".to_owned()));
            };

            // biz/prompt_protection_rule.go:259-264: effective pattern/settings
            // resolved BEFORE save (lo.FromPtrOr / current-settings fallback).
            if let Some(v) = input.name {
                rule.name = v;
            }
            if let Some(v) = input.description {
                rule.description = v;
            }
            if let Some(v) = input.pattern {
                rule.pattern = v;
            }
            if let Some(v) = input.status {
                rule.status = v;
            }
            if let Some(v) = input.settings {
                rule.settings = PromptProtectionSettings::from(v);
            }

            Ok(rule.clone())
        }

        async fn delete_prompt_protection_rule(&self, id: &str) -> Result<(), PromptServiceError> {
            let mut guard = lock(&self.rules);
            let before = guard.len();
            guard.retain(|r| r.id.as_str() != id);
            if guard.len() == before {
                return Err(PromptServiceError::DeleteRule("rule not found".to_owned()));
            }
            Ok(())
        }

        async fn update_prompt_protection_rule_status(
            &self,
            id: &str,
            status: PromptProtectionRuleStatus,
        ) -> Result<PromptProtectionRule, PromptServiceError> {
            let mut guard = lock(&self.rules);
            let Some(rule) = guard.iter_mut().find(|r| r.id.as_str() == id) else {
                return Err(PromptServiceError::UpdateRule("rule not found".to_owned()));
            };
            rule.status = status;
            Ok(rule.clone())
        }

        async fn bulk_delete_prompt_protection_rules(
            &self,
            ids: Vec<String>,
        ) -> Result<(), PromptServiceError> {
            // biz/prompt_protection_rule.go:306-320: empty ids is a no-op.
            if ids.is_empty() {
                return Ok(());
            }
            let mut guard = lock(&self.rules);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            guard.retain(|r| !id_set.contains(r.id.as_str()));
            Ok(())
        }

        async fn bulk_disable_prompt_protection_rules(
            &self,
            ids: Vec<String>,
        ) -> Result<(), PromptServiceError> {
            if ids.is_empty() {
                return Ok(());
            }
            let mut guard = lock(&self.rules);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            for rule in guard.iter_mut() {
                if id_set.contains(rule.id.as_str()) {
                    rule.status = PromptProtectionRuleStatus::Disabled;
                }
            }
            Ok(())
        }

        async fn bulk_enable_prompt_protection_rules(
            &self,
            ids: Vec<String>,
        ) -> Result<(), PromptServiceError> {
            if ids.is_empty() {
                return Ok(());
            }
            let mut guard = lock(&self.rules);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            for rule in guard.iter_mut() {
                if id_set.contains(rule.id.as_str()) {
                    rule.status = PromptProtectionRuleStatus::Enabled;
                }
            }
            Ok(())
        }

        async fn preview_prompt_protection_rule(
            &self,
            input: PromptProtectionRulePreviewInput,
        ) -> Result<PromptProtectionRulePreviewResult, PromptServiceError> {
            *lock(&self.last_preview) = Some(input.clone());
            // Mirror the Go biz/prompt_protection_preview.go semantics with a
            // pure-std-regex subset: the snapshot has no contract for which
            // regex flavour the frontend assumes, so a simple substring replace
            // / contains check is sufficient for resolver-level coverage
            // (the host wires the real regexp2 implementation).
            let has_match = input.test_text.contains(&input.pattern);
            let result = match input.settings.action {
                PromptProtectionAction::Mask => {
                    if has_match {
                        input.test_text.replace(
                            &input.pattern,
                            &input.settings.replacement.unwrap_or_default(),
                        )
                    } else {
                        input.test_text.clone()
                    }
                }
                PromptProtectionAction::Reject => {
                    if has_match {
                        // biz/prompt_protection_preview.go:44-46: result is the
                        // literal string of the action enum value ("reject").
                        "reject".to_owned()
                    } else {
                        input.test_text.clone()
                    }
                }
            };
            Ok(PromptProtectionRulePreviewResult { result, has_match })
        }
    }

    fn schema_with(
        prompt: &InMemoryPromptService,
        rule: &InMemoryPromptProtectionRuleService,
    ) -> AdminSchema {
        let pq: Arc<dyn PromptQueryServices> = Arc::new(prompt.clone());
        let pm: Arc<dyn PromptMutationServices> = Arc::new(prompt.clone());
        let rq: Arc<dyn PromptProtectionRuleQueryServices> = Arc::new(rule.clone());
        let rm: Arc<dyn PromptProtectionRuleMutationServices> = Arc::new(rule.clone());
        admin_schema_builder()
            .data(pq)
            .data(pm)
            .data(rq)
            .data(rm)
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // -----------------------------------------------------------------
    // SDL parity — object types
    // -----------------------------------------------------------------

    #[test]
    fn sdl_prompt_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type Prompt",
            "type Prompt",
            &["projects(…): ProjectConnection!"],
        )?;
        assert!(sdl.contains("type Prompt implements Node {"));
        assert!(snapshot.contains("type Prompt implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_prompt_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type PromptConnection",
            "type PromptConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type PromptEdge", "type PromptEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_prompt_support_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type PromptAction",
            "type PromptActivationCondition",
            "type PromptActivationConditionComposite",
            "type PromptSettings",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_prompt_protection_rule_type_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type PromptProtectionRule",
            "type PromptProtectionRule",
            &[],
        )?;
        assert!(sdl.contains("type PromptProtectionRule implements Node {"));
        assert!(snapshot.contains("type PromptProtectionRule implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_prompt_protection_support_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type PromptProtectionRuleConnection",
            "type PromptProtectionRuleEdge",
            "type PromptProtectionSettings",
            "type PromptProtectionRulePreviewResult",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // SDL parity — input types
    // -----------------------------------------------------------------

    #[test]
    fn sdl_prompt_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input CreatePromptInput",
            "input UpdatePromptInput",
            "input PromptOrder",
            "input PromptActionInput",
            "input PromptActivationConditionInput",
            "input PromptActivationConditionCompositeInput",
            "input PromptSettingsInput",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        // Pin the ASC default exactly (snapshot line 4692).
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "missing ASC default on PromptOrder.direction: {sdl}"
        );
        Ok(())
    }

    #[test]
    fn sdl_prompt_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError>
    {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input PromptWhereInput",
            "input PromptWhereInput",
            &["hasProjectsWith: [ProjectWhereInput!]"],
        )
    }

    #[test]
    fn sdl_prompt_protection_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input CreatePromptProtectionRuleInput",
            "input UpdatePromptProtectionRuleInput",
            "input PromptProtectionRuleOrder",
            "input PromptProtectionSettingsInput",
            "input PromptProtectionRulePreviewInput",
            "input PromptProtectionRuleWhereInput",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "missing ASC default on PromptProtectionRuleOrder.direction: {sdl}"
        );
        Ok(())
    }

    // -----------------------------------------------------------------
    // SDL parity — enums
    // -----------------------------------------------------------------

    #[test]
    fn sdl_prompt_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "enum PromptStatus",
            "enum PromptActionType",
            "enum PromptActivationConditionType",
            "enum PromptOrderField",
            "enum PromptProtectionAction",
            "enum PromptProtectionScope",
            "enum PromptProtectionRuleStatus",
            "enum PromptProtectionRuleOrderField",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // SDL parity — root operation signatures
    // -----------------------------------------------------------------

    #[test]
    fn sdl_queries_and_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query signatures (snapshot type Query, lines 5465-5525).
        assert!(
            sdl.contains(
                "prompts(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: PromptOrder, where: PromptWhereInput): PromptConnection!"
            ),
            "generated SDL missing the prompts connection signature: {sdl}"
        );
        assert!(
            sdl.contains(
                "promptProtectionRules(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: PromptProtectionRuleOrder, where: PromptProtectionRuleWhereInput): \
                 PromptProtectionRuleConnection!"
            ),
            "generated SDL missing the promptProtectionRules connection signature: {sdl}"
        );

        // Mutations (snapshot type Mutation, lines 9271-9277 + 9318-9325).
        for signature in [
            "createPrompt(input: CreatePromptInput!): Prompt!",
            "updatePrompt(id: ID!, input: UpdatePromptInput!): Prompt!",
            "deletePrompt(id: ID!): Boolean!",
            "updatePromptStatus(id: ID!, status: PromptStatus!): Boolean!",
            "bulkDeletePrompts(ids: [ID!]!): Boolean!",
            "bulkEnablePrompts(ids: [ID!]!): Boolean!",
            "bulkDisablePrompts(ids: [ID!]!): Boolean!",
            "createPromptProtectionRule(input: CreatePromptProtectionRuleInput!): \
             PromptProtectionRule!",
            "updatePromptProtectionRule(id: ID!, input: UpdatePromptProtectionRuleInput!): \
             PromptProtectionRule!",
            "deletePromptProtectionRule(id: ID!): Boolean!",
            "updatePromptProtectionRuleStatus(id: ID!, status: \
             PromptProtectionRuleStatus!): Boolean!",
            "bulkDeletePromptProtectionRules(ids: [ID!]!): Boolean!",
            "bulkEnablePromptProtectionRules(ids: [ID!]!): Boolean!",
            "bulkDisablePromptProtectionRules(ids: [ID!]!): Boolean!",
            "previewPromptProtectionRule(input: PromptProtectionRulePreviewInput!): \
             PromptProtectionRulePreviewResult!",
        ] {
            assert!(
                sdl.contains(signature),
                "generated SDL missing `{signature}`: {sdl}"
            );
            assert!(
                snapshot.contains(signature),
                "snapshot missing `{signature}`"
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Ordering lowering
    // -----------------------------------------------------------------

    #[test]
    fn resolve_prompt_order_remaps_created_at_to_id() {
        let selection = resolve_prompt_order(Some(PromptOrder {
            direction: OrderDirection::Desc,
            field: PromptOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(PromptOrderSelection {
                direction: OrderDirection::Desc,
                term: PromptOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_prompt_order_maps_other_fields_one_to_one() {
        for (field, term) in [
            (PromptOrderField::UpdatedAt, PromptOrderTerm::UpdatedAt),
            (PromptOrderField::Order, PromptOrderTerm::Order),
        ] {
            let selection = resolve_prompt_order(Some(PromptOrder {
                direction: OrderDirection::Asc,
                field,
            }));
            assert_eq!(
                selection.map(|s| s.term),
                Some(term),
                "field {field:?} must map to {term:?}"
            );
        }
        assert_eq!(resolve_prompt_order(None), None);
    }

    #[test]
    fn resolve_rule_order_remaps_created_at_to_id() {
        let selection = resolve_prompt_protection_rule_order(Some(PromptProtectionRuleOrder {
            direction: OrderDirection::Desc,
            field: PromptProtectionRuleOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(PromptProtectionRuleOrderSelection {
                direction: OrderDirection::Desc,
                term: PromptProtectionRuleOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_rule_order_maps_other_fields_one_to_one() {
        for (field, term) in [
            (
                PromptProtectionRuleOrderField::UpdatedAt,
                PromptProtectionRuleOrderTerm::UpdatedAt,
            ),
            (
                PromptProtectionRuleOrderField::Name,
                PromptProtectionRuleOrderTerm::Name,
            ),
        ] {
            let selection = resolve_prompt_protection_rule_order(Some(PromptProtectionRuleOrder {
                direction: OrderDirection::Asc,
                field,
            }));
            assert_eq!(
                selection.map(|s| s.term),
                Some(term),
                "field {field:?} must map to {term:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Resolver: createPrompt
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn create_prompt_returns_created_prompt_with_defaults() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    createPrompt(input: {
                        name: "greet",
                        role: "system",
                        content: "hello",
                        settings: { action: { type: prepend } }
                    }) {
                        id name role content status order projectID
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createPrompt"];
        assert_eq!(created["id"], "1");
        assert_eq!(created["name"], "greet");
        assert_eq!(created["role"], "system");
        assert_eq!(created["content"], "hello");
        // ent default: status=enabled, order=0, project_id resolved from ctx.
        assert_eq!(created["status"], "enabled");
        assert_eq!(created["order"], 0);
        assert_eq!(created["projectID"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn create_prompt_duplicate_name_surfaces_go_error_message() {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(1, "dup"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    createPrompt(input: {
                        name: "dup",
                        role: "system",
                        content: "x",
                        settings: { action: { type: prepend } }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("prompt name 'dup' already exists"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: updatePrompt
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_prompt_applies_partial_merge() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(2, "old"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    updatePrompt(id: "2", input: {
                        name: "new",
                        content: "updated"
                    }) { id name content role }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updatePrompt"];
        assert_eq!(updated["id"], "2");
        assert_eq!(updated["name"], "new");
        assert_eq!(updated["content"], "updated");
        // Unset field keeps stored value.
        assert_eq!(updated["role"], "system");
        Ok(())
    }

    #[tokio::test]
    async fn update_prompt_missing_id_surfaces_wrapped_error() {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { updatePrompt(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update prompt"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn update_prompt_duplicate_name_against_other_errors() {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(1, "alpha"));
        lock(&prompt_store.prompts).push(sample_prompt(2, "beta"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { updatePrompt(id: "2", input: { name: "alpha" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("prompt name 'alpha' already exists"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: deletePrompt / updatePromptStatus / bulk*
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn delete_prompt_returns_true_and_removes_row() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(3, "victim"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { deletePrompt(id: "3") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deletePrompt"], true);
        assert!(lock(&prompt_store.prompts).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn update_prompt_status_returns_true_and_changes_status() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(4, "p4"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { updatePromptStatus(id: "4", status: disabled) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updatePromptStatus"], true);
        assert_eq!(
            lock(&prompt_store.prompts)[0].status,
            PromptStatus::Disabled
        );
        Ok(())
    }

    #[tokio::test]
    async fn bulk_enable_prompts_returns_true_and_flips_status() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let mut p = sample_prompt(5, "p5");
        p.status = PromptStatus::Disabled;
        lock(&prompt_store.prompts).push(p);
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { bulkEnablePrompts(ids: ["5"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkEnablePrompts"], true);
        assert_eq!(lock(&prompt_store.prompts)[0].status, PromptStatus::Enabled);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_disable_prompts_returns_true_and_flips_status() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(6, "p6"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { bulkDisablePrompts(ids: ["6"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDisablePrompts"], true);
        assert_eq!(
            lock(&prompt_store.prompts)[0].status,
            PromptStatus::Disabled
        );
        Ok(())
    }

    #[tokio::test]
    async fn bulk_delete_prompts_returns_true_and_removes_matching() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(1, "a"));
        lock(&prompt_store.prompts).push(sample_prompt(2, "b"));
        lock(&prompt_store.prompts).push(sample_prompt(3, "c"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { bulkDeletePrompts(ids: ["1", "3"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDeletePrompts"], true);
        let remaining: Vec<String> = lock(&prompt_store.prompts)
            .iter()
            .map(|p| p.id.to_string())
            .collect();
        assert_eq!(remaining, vec!["2".to_owned()]);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: prompts connection query
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn prompts_returns_connection_with_total_count() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(1, "a"));
        lock(&prompt_store.prompts).push(sample_prompt(2, "b"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"{
                    prompts {
                        totalCount
                        edges { cursor node { id name } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["prompts"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn prompts_created_at_order_remaps_to_id_term() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        lock(&prompt_store.prompts).push(sample_prompt(1, "a"));
        lock(&prompt_store.prompts).push(sample_prompt(2, "b"));
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"{
                    prompts(orderBy: { field: CREATED_AT, direction: DESC }) {
                        edges { node { id } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&prompt_store.captured_query_args).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].order_by,
            Some(PromptOrderSelection {
                direction: OrderDirection::Desc,
                term: PromptOrderTerm::Id,
            })
        );
        let data = resp.data.into_json()?;
        assert_eq!(data["prompts"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn prompts_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ prompts { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("prompt service is not available"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: createPromptProtectionRule
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn create_rule_returns_created_rule_with_defaults() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    createPromptProtectionRule(input: {
                        name: "ssn-mask",
                        pattern: "ssn",
                        settings: { action: mask, replacement: "***", scopes: [user] }
                    }) {
                        id name pattern status settings { action replacement scopes }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createPromptProtectionRule"];
        assert_eq!(created["id"], "1");
        assert_eq!(created["name"], "ssn-mask");
        assert_eq!(created["pattern"], "ssn");
        // ent default: status=enabled.
        assert_eq!(created["status"], "enabled");
        assert_eq!(created["settings"]["action"], "mask");
        assert_eq!(created["settings"]["replacement"], "***");
        assert_eq!(created["settings"]["scopes"][0], "user");
        Ok(())
    }

    #[tokio::test]
    async fn create_rule_duplicate_name_surfaces_go_error_message() {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(1, "dup"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    createPromptProtectionRule(input: {
                        name: "dup",
                        pattern: "x",
                        settings: { action: reject, scopes: [] }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("prompt protection rule name 'dup' already exists"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: updatePromptProtectionRule / delete / status / bulk
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_rule_applies_partial_merge() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(7, "old"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    updatePromptProtectionRule(id: "7", input: {
                        name: "new",
                        pattern: "updated-pattern"
                    }) { id name pattern }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updatePromptProtectionRule"];
        assert_eq!(updated["id"], "7");
        assert_eq!(updated["name"], "new");
        assert_eq!(updated["pattern"], "updated-pattern");
        Ok(())
    }

    #[tokio::test]
    async fn delete_rule_returns_true_and_removes_row() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(5, "victim"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { deletePromptProtectionRule(id: "5") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deletePromptProtectionRule"], true);
        assert!(lock(&rule_store.rules).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn update_rule_status_returns_true_and_flips() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(8, "p8"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    updatePromptProtectionRuleStatus(id: "8", status: archived)
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updatePromptProtectionRuleStatus"], true);
        assert_eq!(
            lock(&rule_store.rules)[0].status,
            PromptProtectionRuleStatus::Archived
        );
        Ok(())
    }

    #[tokio::test]
    async fn bulk_enable_rules_noop_on_empty_ids() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { bulkEnablePromptProtectionRules(ids: []) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkEnablePromptProtectionRules"], true);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_disable_rules_flips_status() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(1, "a"));
        lock(&rule_store.rules).push(sample_rule(2, "b"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(r#"mutation { bulkDisablePromptProtectionRules(ids: ["1", "2"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDisablePromptProtectionRules"], true);
        assert!(
            lock(&rule_store.rules)
                .iter()
                .all(|r| r.status == PromptProtectionRuleStatus::Disabled)
        );
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: previewPromptProtectionRule
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn preview_rule_mask_action_replaces_match() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    previewPromptProtectionRule(input: {
                        pattern: "secret",
                        testText: "my secret token",
                        settings: { action: mask, replacement: "[REDACTED]", scopes: [user] }
                    }) { result hasMatch }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let result = &data["previewPromptProtectionRule"];
        assert_eq!(result["hasMatch"], true);
        assert_eq!(result["result"], "my [REDACTED] token");

        let captured = lock(&rule_store.last_preview).clone();
        let captured = captured.ok_or("preview input not captured")?;
        assert_eq!(captured.pattern, "secret");
        assert_eq!(captured.test_text, "my secret token");
        Ok(())
    }

    #[tokio::test]
    async fn preview_rule_reject_action_returns_reject_literal() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    previewPromptProtectionRule(input: {
                        pattern: "blocked",
                        testText: "this is blocked content",
                        settings: { action: reject, scopes: [system] }
                    }) { result hasMatch }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let result = &data["previewPromptProtectionRule"];
        assert_eq!(result["hasMatch"], true);
        assert_eq!(result["result"], "reject");
        Ok(())
    }

    #[tokio::test]
    async fn preview_rule_no_match_returns_original_text() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"mutation {
                    previewPromptProtectionRule(input: {
                        pattern: "zzz",
                        testText: "nothing here",
                        settings: { action: mask, replacement: "X", scopes: [] }
                    }) { result hasMatch }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let result = &data["previewPromptProtectionRule"];
        assert_eq!(result["hasMatch"], false);
        assert_eq!(result["result"], "nothing here");
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: promptProtectionRules connection query
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn protection_rules_returns_connection_with_total_count() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(1, "a"));
        lock(&rule_store.rules).push(sample_rule(2, "b"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"{
                    promptProtectionRules {
                        totalCount
                        edges { cursor node { id name } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["promptProtectionRules"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn protection_rules_created_at_order_remaps_to_id_term() -> Result<(), TestError> {
        let prompt_store = InMemoryPromptService::default();
        let rule_store = InMemoryPromptProtectionRuleService::default();
        lock(&rule_store.rules).push(sample_rule(1, "a"));
        lock(&rule_store.rules).push(sample_rule(2, "b"));
        let schema = schema_with(&prompt_store, &rule_store);

        let resp = schema
            .execute(
                r#"{
                    promptProtectionRules(orderBy: { field: CREATED_AT, direction: DESC }) {
                        edges { node { id } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&rule_store.captured_query_args).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].order_by,
            Some(PromptProtectionRuleOrderSelection {
                direction: OrderDirection::Desc,
                term: PromptProtectionRuleOrderTerm::Id,
            })
        );
        let data = resp.data.into_json()?;
        assert_eq!(data["promptProtectionRules"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn protection_rules_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(r#"{ promptProtectionRules { totalCount } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("prompt service is not available"),
            "unexpected error: {msg}"
        );
    }
}

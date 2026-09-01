//! ADPT-PROMPT — host adapter wiring the admin GraphQL Prompt domain to live,
//! backend-neutral repository traits.
//!
//! Backs the four host-injected traits declared in
//! `crates/conduit-admin-graphql/src/prompt.rs`:
//!   - [`PromptQueryServices`]                  — `Query.prompts` connection.
//!   - [`PromptMutationServices`]               — the seven Prompt mutations.
//!   - [`PromptProtectionRuleQueryServices`]    — `Query.promptProtectionRules`.
//!   - [`PromptProtectionRuleMutationServices`] — the eight rule mutations.
//!
//! The first two are backed by [`PromptCrudAdapter`] (over [`PromptRepo`]);
//! the protection rule traits are backed by [`PromptProtectionRuleAdapter`]
//! (over [`PromptProtectionRuleRepo`]).
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//!   - `Query.prompts` (`internal/server/gql/ent.resolvers.go:410`): the crate
//!     layer already lowered `orderBy` (`CREATED_AT` → ent `DefaultPromptOrder`
//!     = by id) into [`PromptOrderSelection`]; this adapter materializes the
//!     live rows, filters/sorts in memory and applies Relay offset pagination
//!     (same bounded-materialization strategy as `wiring_channel_crud.rs` —
//!     the prompts table is small).
//!   - `createPrompt` → `biz.PromptService.CreatePrompt` (`biz/prompt.go:126`):
//!     project id from context, settings validation (`biz/prompt.go:88`),
//!     duplicate-name probe scoped to the project
//!     (`xerrors.DuplicateNameError("prompt", name)`), create with ent column
//!     defaults. **Ent default status is `disabled`**
//!     (`internal/ent/schema/prompt.go:58-60` `Default("disabled")`) — the
//!     in-crate mock's `unwrap_or(PromptStatus::Enabled)` comment is wrong
//!     against the Go schema; the repo (and this adapter) follow the schema.
//!   - `updatePrompt` → `biz.PromptService.UpdatePrompt` (`biz/prompt.go:171`):
//!     optional settings validation, duplicate-name probe excluding self, then
//!     the partial merge `SetNillable{Name, Description, Role, Content, Order,
//!     Status}` + conditional `SetSettings`. The `addProjectIDs` /
//!     `removeProjectIDs` / `clearProjects` edge fields are NOT applied by the
//!     Go service — neither here.
//!   - `deletePrompt` → `biz.PromptService.DeletePrompt` (`biz/prompt.go:241`):
//!     the `Prompt` ent schema carries the `SoftDeleteMixin`, so the ent
//!     `Delete()` is intercepted into a soft delete; the repo exposes exactly
//!     that (`soft_delete_prompt_unchecked`).
//!   - `updatePromptStatus` / bulk enable / disable / delete —
//!     `biz/prompt.go:266/302/323/345`: all scope by `(id, project_id)`; the
//!     bulk variants use `IDIn(...)` so missing ids silently match nothing
//!     (no error) — mirrored by ignoring per-id `NotFound`.
//!
//! ## Project scoping (deliberate divergence, documented)
//! Go resolves the project from context (`contexts.GetProjectID`, set by the
//! `X-Project-ID` header middleware) and errors with "project id not found in
//! context" when absent. The Rust admin schema is a boot-time singleton with no
//! per-request project id in its data bag (same situation the channel-extras
//! adapter documents in `wiring.rs`), so mutations pin the bootstrap default
//! project (`DEFAULT_PROJECT_ID` = "1", created by `initialize`). The
//! connection query lists prompts across **all** projects, matching the Go
//! `Query.prompts` ent `Paginate` which applies no project filter; clients
//! scope via `where.projectID`. A future per-request project-scoped path
//! replaces `DEFAULT_PROJECT_ID` here.
//!
//! ## `where` predicate coverage (Query.prompts)
//! Covered: `not`/`and`/`or` recursion, the `projectID` / `order` numeric
//! families, the `name` / `description` / `role` / `content` string families,
//! the `status` enum family, and `hasProjects` (every prompt row has a
//! project, so `true` matches all live rows and `false` none). Deferred
//! (same convention as `wiring_channel_crud.rs`, documented): the `id` and
//! `createdAt` / `updatedAt` predicate families.
//!
//! ## Protection rules — Go parity anchors
//!   - `Query.promptProtectionRules` (`internal/server/gql/ent.resolvers.go:425`):
//!     the crate layer already lowered `orderBy` (`CREATED_AT` → ent default id
//!     order) into [`PromptProtectionRuleOrderSelection`]; this adapter lists
//!     the live rows through `PromptProtectionRuleRepo` (id ASC), filters
//!     / re-orders in memory, and applies Relay offset pagination (rules are a
//!     small global list — Go caches it whole).
//!   - `createPromptProtectionRule` → `biz.PromptProtectionRuleService.CreateRule`
//!     (`biz/prompt_protection_rule.go:221`): `ValidateSettings` first, then the
//!     duplicate-name probe (`xerrors.DuplicateNameError("prompt protection
//!     rule", name)`), then create. **`status` carries
//!     `SkipMutationCreateInput`** — create never sets it and the ent column
//!     default `disabled` applies.
//!   - `updatePromptProtectionRule` → `UpdateRule` (biz:253): `Get` the current
//!     row first ("failed to query prompt protection rule: %w"), resolve the
//!     *effective* pattern/settings (`lo.FromPtrOr` / current-settings
//!     fallback, biz:259-264), validate those, then the partial `SetInput`
//!     patch.
//!   - `deletePromptProtectionRule` → `DeleteRule` (biz:283): the schema
//!     carries `SoftDeleteMixin`, so `DeleteOneID` soft-deletes (repo
//!     `soft_delete_protection_rule_unchecked`), freeing the name under the
//!     `(name, deleted_at)` unique index.
//!   - `updatePromptProtectionRuleStatus` / bulk delete / disable / enable —
//!     biz:293/306/322/339: bulk ops with empty ids are no-ops and use
//!     `IDIn(...)`, so missing ids silently match nothing.
//!   - `previewPromptProtectionRule` → `Preview`
//!     (`biz/prompt_protection_preview.go:21`): validate settings (which
//!     compiles the pattern), match against `testText`; on a match `mask`
//!     replaces every occurrence with `settings.replacement` and `reject`
//!     returns the literal enum string `"reject"`; otherwise the text passes
//!     through unchanged.
//!
//! ## Regex engine divergence (documented, deliberate)
//! Go compiles rule patterns with `regexp2` (backtracking engine —
//! lookarounds, backreferences); this host uses the `regex` crate (RE2-style,
//! no lookarounds/backreferences). A pattern that is valid `regexp2` but not
//! valid `regex` fails compilation here and surfaces the Go validation error
//! wording ("invalid regex pattern: …") — the error is surfaced rather than
//! silently accepting a pattern the gateway could never evaluate. No
//! regexp2-compatible dependency is added.
//!
//! ## Rule `where` predicate coverage (Query.promptProtectionRules)
//! Covered: `not`/`and`/`or` recursion, the `name` / `description` / `pattern`
//! string families, and the `status` enum family. Deferred (same convention as
//! `wiring_channel_crud.rs` and the Prompt families above, documented): the
//! `id` and `createdAt` / `updatedAt` predicate families.
//!
//! ## Remaining deferrals
//!   - Prompt settings validation mirrors the presence checks of
//!     `biz/prompt.go:88-124`; the `xregexp.ValidateRegex(model_pattern)`
//!     compile check is still pending there (PromptCrudAdapter surface is
//!     frozen in this change — follow-up now that `regex` is available).

use std::sync::Arc;

use async_trait::async_trait;

use conduit_admin_graphql::channel::OrderDirection;
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::policy::AdminAccessScope;
use conduit_admin_graphql::prompt::{
    CreatePromptInput, CreatePromptProtectionRuleInput, Prompt as GqlPrompt,
    PromptAction as GqlPromptAction, PromptActionType,
    PromptActivationCondition as GqlPromptActivationCondition,
    PromptActivationConditionComposite as GqlPromptActivationConditionComposite,
    PromptActivationConditionType, PromptConnection, PromptConnectionArgs, PromptEdge,
    PromptMutationServices, PromptOrderTerm, PromptProtectionAction, PromptProtectionRule,
    PromptProtectionRuleConnection, PromptProtectionRuleConnectionArgs, PromptProtectionRuleEdge,
    PromptProtectionRuleMutationServices, PromptProtectionRuleOrderTerm,
    PromptProtectionRulePreviewInput, PromptProtectionRulePreviewResult,
    PromptProtectionRuleQueryServices, PromptProtectionRuleStatus, PromptProtectionRuleWhereInput,
    PromptProtectionScope, PromptProtectionSettings as GqlPromptProtectionSettings,
    PromptProtectionSettingsInput, PromptQueryServices, PromptServiceError,
    PromptSettings as GqlPromptSettings, PromptSettingsInput, PromptStatus, PromptWhereInput,
    UpdatePromptInput, UpdatePromptProtectionRuleInput,
};
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_core::objects::prompt as core_prompt;
use conduit_core::objects::prompt_protection as core_protection;
use conduit_db::repo::PromptRepo;
use conduit_db::repo::prompt_protection_repo::{
    CreateProtectionRuleInput as RepoCreateRuleInput, PromptProtectionRuleRepo,
    RULE_STATUS_DISABLED, RULE_STATUS_ENABLED, UpdateProtectionRuleInput as RepoUpdateRuleInput,
};
use conduit_db::repo::prompt_repo::{
    CreatePromptInput as RepoCreatePromptInput, UpdatePromptInput as RepoUpdatePromptInput,
};
use conduit_db::row::{PromptProtectionRuleRow, PromptRow};
use conduit_db::{PolicyContext, Principal, RepoError, RequestContext};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Bootstrap default project the mutations are scoped to (see module doc,
/// "Project scoping"). Row id of the project `initialize` creates first.
const DEFAULT_PROJECT_ID: &str = "1";

/// GraphQL-facing Prompt domain adapter backed by a live [`PromptRepo`].
/// Implements both [`PromptQueryServices`] and [`PromptMutationServices`].
pub struct PromptCrudAdapter {
    prompt_repo: Arc<dyn PromptRepo>,
    mutation_project_id: String,
}

impl PromptCrudAdapter {
    pub fn new(prompt_repo: Arc<dyn PromptRepo>) -> Self {
        Self {
            prompt_repo,
            mutation_project_id: DEFAULT_PROJECT_ID.to_owned(),
        }
    }

    fn for_access(&self, access: &AdminAccessScope) -> Result<Self, PromptServiceError> {
        let AdminAccessScope::Project(project_id) = access else {
            return Err(PromptServiceError::PermissionDenied(
                "current project is required; send X-Project-ID".to_owned(),
            ));
        };
        let project_id = prompt_db_id(project_id).ok_or_else(|| {
            PromptServiceError::PermissionDenied("authorized project id is invalid".to_owned())
        })?;
        Ok(Self {
            prompt_repo: Arc::clone(&self.prompt_repo),
            mutation_project_id: project_id,
        })
    }

    /// Materialize every live prompt row across all projects. The repo only
    /// lists per project, so the live project ids are enumerated with one raw
    /// query over the repo's pool (established gap-fill precedent —
    /// `wiring_user.rs` does the same for repo-less surfaces) and each project
    /// is then listed through the repo so row decoding stays repo-owned.
    async fn load_all(&self) -> Result<Vec<PromptRow>, PromptServiceError> {
        let ctx = boot_request_context();
        let project_ids = self
            .prompt_repo
            .list_live_prompt_project_ids_unchecked(&ctx)
            .await
            .map_err(|error| PromptServiceError::PromptQuery(error.to_string()))?;

        let mut rows: Vec<PromptRow> = Vec::new();
        for project_id in project_ids {
            let page = self
                .prompt_repo
                .list_prompts_unchecked(&ctx, &project_id)
                .await
                .map_err(|e| PromptServiceError::PromptQuery(e.to_string()))?;
            rows.extend(page);
        }
        Ok(rows)
    }
}

/// The per-request context the host uses for repo calls. Mirrors
/// `wiring::boot_request_context` (a trusted, fully-authorized principal —
/// the admin GraphQL layer performs its own auth before reaching the service).
fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

/// Decode a GraphQL `ID!` (`gid://conduit/Prompt/<n>` wire form or a bare
/// numeric id) into the numeric DB-id string the repo expects. Mirrors Go
/// `GUID.UnmarshalGQL`; a value that is neither is treated as "no such row".
fn prompt_db_id(raw: &str) -> Option<String> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Enum ↔ wire-literal maps
// ---------------------------------------------------------------------------

/// GraphQL `PromptStatus` → the wire literal stored in the `status` column
/// (Go ent enum `enabled | disabled`, `internal/ent/schema/prompt.go:58`).
fn prompt_status_to_wire(status: PromptStatus) -> &'static str {
    match status {
        PromptStatus::Enabled => "enabled",
        PromptStatus::Disabled => "disabled",
    }
}

/// Wire literal → GraphQL `PromptStatus`. The ent enum admits exactly
/// `enabled | disabled` with default `disabled`, so anything else decodes to
/// the schema default.
fn prompt_status_from_wire(status: &str) -> PromptStatus {
    match status {
        "enabled" => PromptStatus::Enabled,
        _ => PromptStatus::Disabled,
    }
}

/// GraphQL `PromptActionType` → the JSON `"type"` literal (Go
/// `objects.PromptActionType`, snake_case wire values).
fn action_type_to_wire(t: PromptActionType) -> &'static str {
    match t {
        PromptActionType::Prepend => core_prompt::action_type::PREPEND,
        PromptActionType::Append => core_prompt::action_type::APPEND,
    }
}

/// JSON `"type"` literal → GraphQL `PromptActionType`. Rows are only ever
/// written through the typed input path; a legacy `{}` settings column
/// deserializes to the zero value (`""`) which maps to `prepend` (the first
/// Go constant), mirroring how the Go force-resolver zero-fills legacy rows.
fn action_type_from_wire(kind: &str) -> PromptActionType {
    match kind {
        core_prompt::action_type::APPEND => PromptActionType::Append,
        _ => PromptActionType::Prepend,
    }
}

/// GraphQL condition type → the JSON `"type"` literal (Go
/// `objects.PromptActivationConditionType`).
fn condition_type_to_wire(t: PromptActivationConditionType) -> &'static str {
    match t {
        PromptActivationConditionType::ModelId => core_prompt::activation_condition_type::MODEL_ID,
        PromptActivationConditionType::ModelPattern => {
            core_prompt::activation_condition_type::MODEL_PATTERN
        }
        PromptActivationConditionType::ApiKey => core_prompt::activation_condition_type::API_KEY,
    }
}

/// JSON `"type"` literal → GraphQL condition type (zero-value fallback as for
/// [`action_type_from_wire`]).
fn condition_type_from_wire(kind: &str) -> PromptActivationConditionType {
    match kind {
        core_prompt::activation_condition_type::MODEL_PATTERN => {
            PromptActivationConditionType::ModelPattern
        }
        core_prompt::activation_condition_type::API_KEY => PromptActivationConditionType::ApiKey,
        _ => PromptActivationConditionType::ModelId,
    }
}

// ---------------------------------------------------------------------------
// Settings validation — mirrors `PromptService.ValidatePromptSettings`
// (biz/prompt.go:88-124). Error strings are the Go originals; the crate's
// `PromptServiceError::InvalidPromptSettings` adds its declared prefix.
// ---------------------------------------------------------------------------

fn validate_prompt_settings(settings: &PromptSettingsInput) -> Result<(), PromptServiceError> {
    // Go: `if len(settings.Conditions) == 0 { return nil }`.
    let Some(composites) = &settings.conditions else {
        return Ok(());
    };

    for composite in composites {
        let Some(conditions) = &composite.conditions else {
            continue;
        };
        for condition in conditions {
            match condition.condition_type {
                PromptActivationConditionType::ModelPattern => {
                    if condition.model_pattern.as_deref().is_none_or(str::is_empty) {
                        return Err(PromptServiceError::InvalidPromptSettings(
                            "model_pattern is required when type is model_pattern".to_owned(),
                        ));
                    }
                    // DEFER: Go additionally compiles the pattern
                    // (`xregexp.ValidateRegex`, biz/prompt.go:100); conduit-bin
                    // has no regex dependency, so the compile check is pending.
                }
                PromptActivationConditionType::ModelId => {
                    if condition.model_id.as_deref().is_none_or(str::is_empty) {
                        return Err(PromptServiceError::InvalidPromptSettings(
                            "model_id is required when type is model_id".to_owned(),
                        ));
                    }
                }
                PromptActivationConditionType::ApiKey => match condition.api_key_id {
                    None => {
                        return Err(PromptServiceError::InvalidPromptSettings(
                            "api_key_id is required when type is api_key".to_owned(),
                        ));
                    }
                    Some(id) if id <= 0 => {
                        return Err(PromptServiceError::InvalidPromptSettings(
                            "api_key_id must be greater than 0".to_owned(),
                        ));
                    }
                    Some(_) => {}
                },
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GraphQL input ↔ settings JSON column (via the `conduit-core` typed objects,
// whose serde tags mirror Go `objects.PromptSettings` exactly: `type` /
// `model_id` / `model_pattern` / `api_key_id`, snake_case).
// ---------------------------------------------------------------------------

fn settings_input_to_json(settings: PromptSettingsInput) -> Value {
    let core = core_prompt::PromptSettings {
        action: core_prompt::PromptAction {
            kind: action_type_to_wire(settings.action.action_type).to_string(),
        },
        conditions: settings
            .conditions
            .unwrap_or_default()
            .into_iter()
            .map(
                |composite| core_prompt::PromptActivationConditionComposite {
                    conditions: composite
                        .conditions
                        .unwrap_or_default()
                        .into_iter()
                        .map(|condition| core_prompt::PromptActivationCondition {
                            kind: condition_type_to_wire(condition.condition_type).to_string(),
                            model_id: condition.model_id,
                            model_pattern: condition.model_pattern,
                            api_key_id: condition.api_key_id,
                        })
                        .collect(),
                },
            )
            .collect(),
    };
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

/// Settings JSON column → GraphQL `PromptSettings`. Empty `conditions` maps to
/// GraphQL `null` (Go `omitempty` — nil slice never serializes).
fn settings_json_to_gql(value: Value) -> GqlPromptSettings {
    let core: core_prompt::PromptSettings = serde_json::from_value(value).unwrap_or_default();
    GqlPromptSettings {
        action: GqlPromptAction {
            action_type: action_type_from_wire(&core.action.kind),
        },
        conditions: if core.conditions.is_empty() {
            None
        } else {
            Some(
                core.conditions
                    .into_iter()
                    .map(|composite| GqlPromptActivationConditionComposite {
                        conditions: if composite.conditions.is_empty() {
                            None
                        } else {
                            Some(
                                composite
                                    .conditions
                                    .into_iter()
                                    .map(|condition| GqlPromptActivationCondition {
                                        condition_type: condition_type_from_wire(&condition.kind),
                                        model_id: condition.model_id,
                                        model_pattern: condition.model_pattern,
                                        api_key_id: condition.api_key_id,
                                    })
                                    .collect(),
                            )
                        },
                    })
                    .collect(),
            )
        },
    }
}

/// Convert a `PromptRow` into the GraphQL `Prompt`. The Node id carries the
/// `gid://conduit/Prompt/<n>` wire form (Go `objects.GUID`, same shaping as
/// `conv::channel_row_to_gql`).
pub(crate) fn prompt_row_to_gql(row: PromptRow) -> GqlPrompt {
    GqlPrompt {
        id: format!("gid://conduit/Prompt/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        // The column is INTEGER; the repo CASTs it to TEXT for the row type.
        project_id: row.project_id.parse::<i64>().unwrap_or_default(),
        name: row.name,
        description: row.description,
        role: row.role,
        content: row.content,
        status: prompt_status_from_wire(&row.status),
        order: row.order_val,
        settings: settings_json_to_gql(row.settings),
    }
}

// ---------------------------------------------------------------------------
// PromptQueryServices — Query.prompts
// ---------------------------------------------------------------------------

#[async_trait]
impl PromptQueryServices for PromptCrudAdapter {
    async fn prompts(
        &self,
        args: PromptConnectionArgs,
    ) -> Result<PromptConnection, PromptServiceError> {
        let rows = self.load_all().await?;

        // `where` filter (in-memory; see module doc for covered families).
        let mut rows: Vec<PromptRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| prompt_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Baseline order: ent's `DefaultPromptOrder` (by id ASC) — the Go
        // Paginate default when no orderBy is supplied. The repo returned
        // per-project `order ASC` batches, so a re-sort is always required.
        rows.sort_by_key(|r| r.id.parse::<i64>().unwrap_or(i64::MAX));

        // Explicit ordering: the crate already lowered `CREATED_AT` → `Id`
        // (ent.resolvers.go:415-417); the other terms map one-to-one.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    PromptOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    PromptOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                    PromptOrderTerm::Order => a.order_val.cmp(&b.order_val),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let prompts: Vec<GqlPrompt> = rows.into_iter().map(prompt_row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme (matching
        // `connection_from_offset_page`; `before`/`last` are not used by the
        // admin frontend and are ignored, same as the sibling adapters). A
        // malformed `after` degrades to offset 0 rather than failing the query.
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset)
            .unwrap_or(0)
            .min(prompts.len());
        let windowed = prompts[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

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

    async fn prompts_with_access(
        &self,
        access: &AdminAccessScope,
        mut args: PromptConnectionArgs,
    ) -> Result<PromptConnection, PromptServiceError> {
        if let AdminAccessScope::Project(project_id) = access {
            let project_id = prompt_db_id(project_id)
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or_else(|| {
                    PromptServiceError::PermissionDenied(
                        "authorized project id is invalid".to_owned(),
                    )
                })?;
            let caller_filter = args.where_filter.take();
            args.where_filter = Some(PromptWhereInput {
                project_id: Some(project_id),
                and: caller_filter.map(|filter| vec![filter]),
                ..Default::default()
            });
        }
        self.prompts(args).await
    }
}

// ---------------------------------------------------------------------------
// PromptMutationServices — create / update / delete / status / bulk*
// ---------------------------------------------------------------------------

#[async_trait]
impl PromptMutationServices for PromptCrudAdapter {
    async fn create_prompt(
        &self,
        input: CreatePromptInput,
    ) -> Result<GqlPrompt, PromptServiceError> {
        // biz/prompt.go:132: settings validated before the duplicate probe.
        validate_prompt_settings(&input.settings)?;

        let ctx = boot_request_context();
        // Retain the name for the duplicate-name error (repo `NameConflict`
        // maps to Go `xerrors.DuplicateNameError("prompt", …)`).
        let name = input.name.clone();

        let repo_input = RepoCreatePromptInput {
            // PostgreSQL owns the generated PK; `id` is ignored on
            // insert (read-back uses the DB id).
            id: String::new(),
            // Go: project id from context (module doc, "Project scoping").
            // `input.projectIDs` is the pending `projects(...)` edge field and
            // is NOT applied by the Go service either.
            project_id: self.mutation_project_id.clone(),
            name: input.name,
            description: input.description,
            role: input.role,
            content: input.content,
            // ent column default when nil: status = disabled, order = 0.
            status: input.status.map(|s| prompt_status_to_wire(s).to_string()),
            order: input.order,
            settings: Some(settings_input_to_json(input.settings)),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let row = self
            .prompt_repo
            .create_prompt_unchecked(&ctx, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => PromptServiceError::DuplicatePromptName(name),
                other => PromptServiceError::CreatePrompt(other.to_string()),
            })?;
        Ok(prompt_row_to_gql(row))
    }

    async fn update_prompt(
        &self,
        id: &str,
        input: UpdatePromptInput,
    ) -> Result<GqlPrompt, PromptServiceError> {
        // biz/prompt.go:177-181: settings validated only when supplied.
        if let Some(settings) = &input.settings {
            validate_prompt_settings(settings)?;
        }

        let ctx = boot_request_context();
        let db_id = prompt_db_id(id).ok_or_else(|| {
            PromptServiceError::UpdatePrompt(PromptServiceError::PromptNotFound.to_string())
        })?;
        // Name (if any) retained for the duplicate-name error.
        let name = input.name.clone();

        // Field application mirrors Go biz/prompt.go:201-215:
        // SetNillable{Name, Description, Role, Content, Order, Status} +
        // conditional SetSettings. The project-edge fields (addProjectIDs /
        // removeProjectIDs / clearProjects) are not applied by the Go service.
        let repo_input = RepoUpdatePromptInput {
            name: input.name,
            description: input.description,
            role: input.role,
            content: input.content,
            order: input.order,
            status: input.status.map(|s| prompt_status_to_wire(s).to_string()),
            settings: input.settings.map(settings_input_to_json),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let row = self
            .prompt_repo
            .update_prompt_unchecked(&ctx, &self.mutation_project_id, &db_id, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => {
                    PromptServiceError::DuplicatePromptName(name.unwrap_or_default())
                }
                // biz/prompt.go:222-224: zero rows updated → "prompt not found
                // or not in project", surfaced through the crate's wrapping
                // variant (matching the in-crate service double).
                RepoError::NotFound(_) => {
                    PromptServiceError::UpdatePrompt(PromptServiceError::PromptNotFound.to_string())
                }
                other => PromptServiceError::UpdatePrompt(other.to_string()),
            })?;
        Ok(prompt_row_to_gql(row))
    }

    async fn delete_prompt(&self, id: &str) -> Result<(), PromptServiceError> {
        let ctx = boot_request_context();
        let db_id = prompt_db_id(id).ok_or_else(|| {
            PromptServiceError::DeletePrompt(PromptServiceError::PromptNotFound.to_string())
        })?;
        // Soft delete (ent SoftDeleteMixin) — hides the row from every default
        // query, which is the observable admin-UI effect of Go's Delete().
        self.prompt_repo
            .soft_delete_prompt_unchecked(
                &ctx,
                &self.mutation_project_id,
                &db_id,
                chrono::Utc::now().to_rfc3339(),
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => {
                    PromptServiceError::DeletePrompt(PromptServiceError::PromptNotFound.to_string())
                }
                other => PromptServiceError::DeletePrompt(other.to_string()),
            })?;
        Ok(())
    }

    async fn update_prompt_status(
        &self,
        id: &str,
        status: PromptStatus,
    ) -> Result<GqlPrompt, PromptServiceError> {
        let ctx = boot_request_context();
        let db_id = prompt_db_id(id).ok_or_else(|| {
            PromptServiceError::UpdatePrompt(PromptServiceError::PromptNotFound.to_string())
        })?;
        let row = self
            .prompt_repo
            .set_prompt_status_unchecked(
                &ctx,
                &self.mutation_project_id,
                &db_id,
                prompt_status_to_wire(status).to_string(),
                chrono::Utc::now().to_rfc3339(),
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => {
                    PromptServiceError::UpdatePrompt(PromptServiceError::PromptNotFound.to_string())
                }
                other => PromptServiceError::UpdatePrompt(other.to_string()),
            })?;
        Ok(prompt_row_to_gql(row))
    }

    async fn bulk_delete_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
        // biz/prompt.go:302-321: `IDIn(ids…)` — ids that match nothing are
        // silently skipped; only real backend failures surface.
        let ctx = boot_request_context();
        for id in ids {
            let Some(db_id) = prompt_db_id(&id) else {
                continue;
            };
            match self
                .prompt_repo
                .soft_delete_prompt_unchecked(
                    &ctx,
                    &self.mutation_project_id,
                    &db_id,
                    chrono::Utc::now().to_rfc3339(),
                )
                .await
            {
                Ok(_) | Err(RepoError::NotFound(_)) => {}
                Err(other) => {
                    return Err(PromptServiceError::DeletePrompt(other.to_string()));
                }
            }
        }
        Ok(())
    }

    async fn bulk_enable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
        self.bulk_set_status(ids, PromptStatus::Enabled).await
    }

    async fn bulk_disable_prompts(&self, ids: Vec<String>) -> Result<(), PromptServiceError> {
        self.bulk_set_status(ids, PromptStatus::Disabled).await
    }

    async fn create_prompt_with_access(
        &self,
        access: &AdminAccessScope,
        input: CreatePromptInput,
    ) -> Result<GqlPrompt, PromptServiceError> {
        self.for_access(access)?.create_prompt(input).await
    }

    async fn update_prompt_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        input: UpdatePromptInput,
    ) -> Result<GqlPrompt, PromptServiceError> {
        self.for_access(access)?.update_prompt(id, input).await
    }

    async fn delete_prompt_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
    ) -> Result<(), PromptServiceError> {
        self.for_access(access)?.delete_prompt(id).await
    }

    async fn update_prompt_status_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        status: PromptStatus,
    ) -> Result<GqlPrompt, PromptServiceError> {
        self.for_access(access)?
            .update_prompt_status(id, status)
            .await
    }

    async fn bulk_delete_prompts_with_access(
        &self,
        access: &AdminAccessScope,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        self.for_access(access)?.bulk_delete_prompts(ids).await
    }

    async fn bulk_enable_prompts_with_access(
        &self,
        access: &AdminAccessScope,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        self.for_access(access)?.bulk_enable_prompts(ids).await
    }

    async fn bulk_disable_prompts_with_access(
        &self,
        access: &AdminAccessScope,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        self.for_access(access)?.bulk_disable_prompts(ids).await
    }
}

impl PromptCrudAdapter {
    /// Shared body of `bulkEnablePrompts` / `bulkDisablePrompts`
    /// (biz/prompt.go:323 / :345): `IDIn(ids…).SetStatus(…)` — missing ids
    /// match nothing and are not an error.
    async fn bulk_set_status(
        &self,
        ids: Vec<String>,
        status: PromptStatus,
    ) -> Result<(), PromptServiceError> {
        let ctx = boot_request_context();
        for id in ids {
            let Some(db_id) = prompt_db_id(&id) else {
                continue;
            };
            match self
                .prompt_repo
                .set_prompt_status_unchecked(
                    &ctx,
                    &self.mutation_project_id,
                    &db_id,
                    prompt_status_to_wire(status).to_string(),
                    chrono::Utc::now().to_rfc3339(),
                )
                .await
            {
                Ok(_) | Err(RepoError::NotFound(_)) => {}
                Err(other) => {
                    return Err(PromptServiceError::UpdatePrompt(other.to_string()));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// `where` predicate evaluation (Query.prompts)
// ---------------------------------------------------------------------------

/// Whether a `PromptRow` satisfies a `PromptWhereInput` predicate tree.
/// `not`/`and`/`or` recurse; an empty `and` matches (ent semantics) and an
/// empty `or` is ignored so it never blacks out the whole result.
/// Deferred families: `id`, `createdAt`, `updatedAt` (module doc).
fn prompt_row_matches_where(row: &PromptRow, w: &PromptWhereInput) -> bool {
    if let Some(inner) = &w.not
        && prompt_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| prompt_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| prompt_row_matches_where(row, c))
    {
        return false;
    }

    // projectID numeric family (the row column is INTEGER, CAST to TEXT).
    let project_id = row.project_id.parse::<i64>().unwrap_or_default();
    if !i64_family(
        project_id,
        &w.project_id,
        &w.project_id_neq,
        &w.project_id_in,
        &w.project_id_not_in,
        &w.project_id_gt,
        &w.project_id_gte,
        &w.project_id_lt,
        &w.project_id_lte,
    ) {
        return false;
    }

    // name string family
    if !str_family(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_gt,
        &w.name_gte,
        &w.name_lt,
        &w.name_lte,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
        &w.name_equal_fold,
        &w.name_contains_fold,
    ) {
        return false;
    }

    // description string family
    if !str_family(
        &row.description,
        &w.description,
        &w.description_neq,
        &w.description_in,
        &w.description_not_in,
        &w.description_gt,
        &w.description_gte,
        &w.description_lt,
        &w.description_lte,
        &w.description_contains,
        &w.description_has_prefix,
        &w.description_has_suffix,
        &w.description_equal_fold,
        &w.description_contains_fold,
    ) {
        return false;
    }

    // role string family
    if !str_family(
        &row.role,
        &w.role,
        &w.role_neq,
        &w.role_in,
        &w.role_not_in,
        &w.role_gt,
        &w.role_gte,
        &w.role_lt,
        &w.role_lte,
        &w.role_contains,
        &w.role_has_prefix,
        &w.role_has_suffix,
        &w.role_equal_fold,
        &w.role_contains_fold,
    ) {
        return false;
    }

    // content string family
    if !str_family(
        &row.content,
        &w.content,
        &w.content_neq,
        &w.content_in,
        &w.content_not_in,
        &w.content_gt,
        &w.content_gte,
        &w.content_lt,
        &w.content_lte,
        &w.content_contains,
        &w.content_has_prefix,
        &w.content_has_suffix,
        &w.content_equal_fold,
        &w.content_contains_fold,
    ) {
        return false;
    }

    // status enum predicates
    if let Some(s) = w.status
        && row.status != prompt_status_to_wire(s)
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row.status == prompt_status_to_wire(s)
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list.iter().any(|s| row.status == prompt_status_to_wire(*s))
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list.iter().any(|s| row.status == prompt_status_to_wire(*s))
    {
        return false;
    }

    // order numeric family
    if !i64_family(
        row.order_val,
        &w.order,
        &w.order_neq,
        &w.order_in,
        &w.order_not_in,
        &w.order_gt,
        &w.order_gte,
        &w.order_lt,
        &w.order_lte,
    ) {
        return false;
    }

    // hasProjects existence: `project_id` is NOT NULL in the ent schema, so
    // every live prompt has its project — `true` matches all, `false` none.
    if w.has_projects == Some(false) {
        return false;
    }

    true
}

/// Evaluate the full string-predicate family (eq/neq/in/notIn/gt/gte/lt/lte/
/// contains/hasPrefix/hasSuffix/equalFold/containsFold) against a column
/// value. `None` predicates are skipped (AND semantics, matching ent).
#[allow(clippy::too_many_arguments)]
fn str_family(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    gt: &Option<String>,
    gte: &Option<String>,
    lt: &Option<String>,
    lte: &Option<String>,
    contains: &Option<String>,
    has_prefix: &Option<String>,
    has_suffix: &Option<String>,
    equal_fold: &Option<String>,
    contains_fold: &Option<String>,
) -> bool {
    if let Some(v) = eq
        && value != v
    {
        return false;
    }
    if let Some(v) = neq
        && value == v
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(v) = gt
        && value <= v.as_str()
    {
        return false;
    }
    if let Some(v) = gte
        && value < v.as_str()
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v.as_str()
    {
        return false;
    }
    if let Some(v) = lte
        && value > v.as_str()
    {
        return false;
    }
    if let Some(v) = contains
        && !value.contains(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_prefix
        && !value.starts_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_suffix
        && !value.ends_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = equal_fold
        && !value.eq_ignore_ascii_case(v)
    {
        return false;
    }
    if let Some(v) = contains_fold
        && !value.to_lowercase().contains(&v.to_lowercase())
    {
        return false;
    }
    true
}

/// Evaluate the numeric-predicate family (eq/neq/in/notIn/gt/gte/lt/lte) for
/// an `i64` column. `None` predicates are skipped (AND semantics).
#[allow(clippy::too_many_arguments)]
fn i64_family(
    value: i64,
    eq: &Option<i64>,
    neq: &Option<i64>,
    in_set: &Option<Vec<i64>>,
    not_in: &Option<Vec<i64>>,
    gt: &Option<i64>,
    gte: &Option<i64>,
    lt: &Option<i64>,
    lte: &Option<i64>,
) -> bool {
    if let Some(v) = eq
        && value != *v
    {
        return false;
    }
    if let Some(v) = neq
        && value == *v
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.contains(&value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.contains(&value)
    {
        return false;
    }
    if let Some(v) = gt
        && value <= *v
    {
        return false;
    }
    if let Some(v) = gte
        && value < *v
    {
        return false;
    }
    if let Some(v) = lt
        && value >= *v
    {
        return false;
    }
    if let Some(v) = lte
        && value > *v
    {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// PromptProtectionRule adapter — backed by PromptProtectionRuleRepo.
// ---------------------------------------------------------------------------

/// GraphQL-facing PromptProtectionRule domain adapter backed by the live
/// [`PromptProtectionRuleRepo`]. Implements both
/// [`PromptProtectionRuleQueryServices`] and
/// [`PromptProtectionRuleMutationServices`]. Rules are a **global** surface
/// (no project scoping — the ent schema has no project field/edge), so unlike
/// [`PromptCrudAdapter`] there is no `DEFAULT_PROJECT_ID` pinning here.
pub struct PromptProtectionRuleAdapter {
    rule_repo: Arc<dyn PromptProtectionRuleRepo>,
}

impl PromptProtectionRuleAdapter {
    pub fn new(rule_repo: Arc<dyn PromptProtectionRuleRepo>) -> Self {
        Self { rule_repo }
    }

    /// Shared body of `bulkEnablePromptProtectionRules` /
    /// `bulkDisablePromptProtectionRules` (biz/prompt_protection_rule.go:322 /
    /// :339): empty ids are a no-op, `IDIn(ids…)` skips missing rows.
    async fn bulk_set_rule_status(
        &self,
        ids: Vec<String>,
        status: &str,
    ) -> Result<(), PromptServiceError> {
        // Undecodable ids cannot exist in the integer-keyed table — dropped,
        // matching Go where a malformed GUID never reaches the biz layer as a
        // matching int id.
        let db_ids: Vec<String> = ids.iter().filter_map(|id| rule_db_id(id)).collect();
        if db_ids.is_empty() {
            return Ok(());
        }
        let ctx = boot_request_context();
        self.rule_repo
            .bulk_set_protection_rule_status_unchecked(&ctx, &db_ids, status)
            .await
            .map_err(|e| PromptServiceError::UpdateRule(e.to_string()))?;
        Ok(())
    }
}

/// Decode a rule `ID!` — same wire forms as [`prompt_db_id`], with the
/// `gid://conduit/PromptProtectionRule/<n>` type tag.
fn rule_db_id(raw: &str) -> Option<String> {
    prompt_db_id(raw)
}

// ---------------------------------------------------------------------------
// Rule enum ↔ wire-literal maps
// ---------------------------------------------------------------------------

/// GraphQL `PromptProtectionRuleStatus` → the `status` column literal (Go ent
/// enum `enabled | disabled | archived`, `prompt_protection_rule.go:44-47`).
fn rule_status_to_wire(status: PromptProtectionRuleStatus) -> &'static str {
    match status {
        PromptProtectionRuleStatus::Enabled => "enabled",
        PromptProtectionRuleStatus::Disabled => "disabled",
        PromptProtectionRuleStatus::Archived => "archived",
    }
}

/// Wire literal → GraphQL `PromptProtectionRuleStatus`. Anything outside the
/// closed ent set decodes to the schema default `disabled`.
fn rule_status_from_wire(status: &str) -> PromptProtectionRuleStatus {
    match status {
        "enabled" => PromptProtectionRuleStatus::Enabled,
        "archived" => PromptProtectionRuleStatus::Archived,
        _ => PromptProtectionRuleStatus::Disabled,
    }
}

/// GraphQL `PromptProtectionAction` → the JSON `"action"` literal (Go
/// `objects.PromptProtectionAction`).
fn protection_action_to_wire(action: PromptProtectionAction) -> &'static str {
    match action {
        PromptProtectionAction::Mask => core_protection::PROMPT_PROTECTION_ACTION_MASK,
        PromptProtectionAction::Reject => core_protection::PROMPT_PROTECTION_ACTION_REJECT,
    }
}

/// JSON `"action"` literal → GraphQL enum. Rows are only written through the
/// typed input path; a legacy zero value (`""`) maps to `mask` (the first Go
/// constant), the same zero-fill convention as [`action_type_from_wire`].
fn protection_action_from_wire(action: &str) -> PromptProtectionAction {
    match action {
        core_protection::PROMPT_PROTECTION_ACTION_REJECT => PromptProtectionAction::Reject,
        _ => PromptProtectionAction::Mask,
    }
}

/// GraphQL `PromptProtectionScope` → the JSON scope literal (Go
/// `objects.PromptProtectionScope`).
fn protection_scope_to_wire(scope: PromptProtectionScope) -> &'static str {
    match scope {
        PromptProtectionScope::System => core_protection::PROMPT_PROTECTION_SCOPE_SYSTEM,
        PromptProtectionScope::Developer => core_protection::PROMPT_PROTECTION_SCOPE_DEVELOPER,
        PromptProtectionScope::User => core_protection::PROMPT_PROTECTION_SCOPE_USER,
        PromptProtectionScope::Assistant => core_protection::PROMPT_PROTECTION_SCOPE_ASSISTANT,
        PromptProtectionScope::Tool => core_protection::PROMPT_PROTECTION_SCOPE_TOOL,
    }
}

/// JSON scope literal → GraphQL enum. The closed GraphQL enum cannot carry an
/// unknown literal; unknowns (impossible through the typed write path — the
/// Go biz layer rejects them too, `ValidateSettings` biz:109-120) are dropped.
fn protection_scope_from_wire(scope: &str) -> Option<PromptProtectionScope> {
    match scope {
        core_protection::PROMPT_PROTECTION_SCOPE_SYSTEM => Some(PromptProtectionScope::System),
        core_protection::PROMPT_PROTECTION_SCOPE_DEVELOPER => {
            Some(PromptProtectionScope::Developer)
        }
        core_protection::PROMPT_PROTECTION_SCOPE_USER => Some(PromptProtectionScope::User),
        core_protection::PROMPT_PROTECTION_SCOPE_ASSISTANT => {
            Some(PromptProtectionScope::Assistant)
        }
        core_protection::PROMPT_PROTECTION_SCOPE_TOOL => Some(PromptProtectionScope::Tool),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rule settings — GraphQL input ↔ settings JSON column (via the typed
// `conduit-core` object whose serde tags mirror Go
// `objects.PromptProtectionSettings` exactly: `action` / `replacement` /
// `scopes` with omitempty).
// ---------------------------------------------------------------------------

/// GraphQL settings input → the typed core object. Go's `replacement` is a
/// plain `string` with `omitempty`, so an empty string and an absent field are
/// the same wire state — normalized to `None` here.
fn protection_settings_input_to_core(
    input: PromptProtectionSettingsInput,
) -> core_protection::PromptProtectionSettings {
    core_protection::PromptProtectionSettings {
        action: protection_action_to_wire(input.action).to_string(),
        replacement: input.replacement.filter(|r| !r.is_empty()),
        scopes: input
            .scopes
            .unwrap_or_default()
            .into_iter()
            .map(|s| protection_scope_to_wire(s).to_string())
            .collect(),
    }
}

/// Typed core settings → the JSON column value.
fn protection_settings_core_to_json(core: &core_protection::PromptProtectionSettings) -> Value {
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

/// Settings JSON column → GraphQL `PromptProtectionSettings`. Missing
/// `scopes` zero-fills to an empty list (the Go read-side force-resolver does
/// the same for legacy rows).
fn protection_settings_json_to_gql(value: Value) -> GqlPromptProtectionSettings {
    let core: core_protection::PromptProtectionSettings =
        serde_json::from_value(value).unwrap_or_default();
    GqlPromptProtectionSettings {
        action: protection_action_from_wire(&core.action),
        replacement: core.replacement,
        scopes: core
            .scopes
            .iter()
            .filter_map(|s| protection_scope_from_wire(s))
            .collect(),
    }
}

/// Convert a `PromptProtectionRuleRow` into the GraphQL node. The Node id
/// carries the `gid://conduit/PromptProtectionRule/<n>` wire form (Go
/// `objects.GUID`).
pub(crate) fn rule_row_to_gql(row: PromptProtectionRuleRow) -> PromptProtectionRule {
    PromptProtectionRule {
        id: format!("gid://conduit/PromptProtectionRule/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        description: row.description,
        pattern: row.pattern,
        status: rule_status_from_wire(&row.status),
        settings: protection_settings_json_to_gql(row.settings),
    }
}

// ---------------------------------------------------------------------------
// Rule settings validation — mirrors `PromptProtectionRuleService.
// ValidateSettings` (biz/prompt_protection_rule.go:92-123). Error strings are
// the Go originals; the crate's `PromptServiceError::InvalidRuleSettings`
// adds its declared "invalid prompt protection settings: " prefix.
// ---------------------------------------------------------------------------

/// Validate a (pattern, settings) pair and return the compiled pattern (the
/// Go service caches the compile; `Preview` reuses it for matching).
///
/// Regex divergence (module doc): Go compiles with `regexp2`
/// (lookarounds/backreferences); the `regex` crate rejects those, so such
/// patterns surface the Go "invalid regex pattern" wording here even though
/// regexp2 would accept them.
fn validate_rule_settings(
    pattern: &str,
    settings: &core_protection::PromptProtectionSettings,
) -> Result<regex::Regex, PromptServiceError> {
    // Go biz:93-95 — compile first.
    let re = regex::Regex::new(pattern).map_err(|err| {
        PromptServiceError::InvalidRuleSettings(format!("invalid regex pattern: {err}"))
    })?;

    // Go biz:97-99 ("settings are required") is unreachable here: the GraphQL
    // input carries a non-null settings object.

    // Go biz:101-103 — closed action set. Only reachable for a stored legacy
    // settings column (the GraphQL enum is closed).
    if settings.action != core_protection::PROMPT_PROTECTION_ACTION_MASK
        && settings.action != core_protection::PROMPT_PROTECTION_ACTION_REJECT
    {
        return Err(PromptServiceError::InvalidRuleSettings(format!(
            "invalid action: {}",
            settings.action
        )));
    }

    // Go biz:105-107 — mask needs a replacement (Go compares against "").
    if settings.action == core_protection::PROMPT_PROTECTION_ACTION_MASK
        && settings.replacement.as_deref().unwrap_or("").is_empty()
    {
        return Err(PromptServiceError::InvalidRuleSettings(
            "replacement is required for mask action".to_owned(),
        ));
    }

    // Go biz:109-120 — every scope must be one of the five known literals.
    for scope in &settings.scopes {
        if protection_scope_from_wire(scope).is_none() {
            return Err(PromptServiceError::InvalidRuleSettings(format!(
                "invalid scope: {scope}"
            )));
        }
    }

    Ok(re)
}

// ---------------------------------------------------------------------------
// PromptProtectionRuleQueryServices — Query.promptProtectionRules
// ---------------------------------------------------------------------------

#[async_trait]
impl PromptProtectionRuleQueryServices for PromptProtectionRuleAdapter {
    async fn prompt_protection_rules(
        &self,
        args: PromptProtectionRuleConnectionArgs,
    ) -> Result<PromptProtectionRuleConnection, PromptServiceError> {
        let ctx = boot_request_context();
        // Repo returns live rows id ASC — exactly ent's default order, which
        // is also the baseline when no orderBy is supplied (and what the
        // crate lowered `CREATED_AT` to).
        let rows = self
            .rule_repo
            .list_protection_rules_unchecked(&ctx)
            .await
            .map_err(|e| PromptServiceError::RuleQueryList(e.to_string()))?;

        // `where` filter (in-memory; covered families in the module doc).
        let mut rows: Vec<PromptProtectionRuleRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| rule_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Explicit ordering: the crate lowered `CREATED_AT` → `Id`
        // (ent.resolvers.go:427-429); UPDATED_AT / NAME map one-to-one.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    PromptProtectionRuleOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    PromptProtectionRuleOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                    PromptProtectionRuleOrderTerm::Name => a.name.cmp(&b.name),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let nodes: Vec<PromptProtectionRule> = rows.into_iter().map(rule_row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme (identical
        // strategy to `PromptCrudAdapter::prompts`; `before`/`last` are not
        // used by the admin frontend and are ignored).
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset).unwrap_or(0).min(nodes.len());
        let windowed = nodes[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

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

// ---------------------------------------------------------------------------
// PromptProtectionRuleMutationServices — create / update / delete / status /
// bulk* / preview
// ---------------------------------------------------------------------------

#[async_trait]
impl PromptProtectionRuleMutationServices for PromptProtectionRuleAdapter {
    async fn create_prompt_protection_rule(
        &self,
        input: CreatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, PromptServiceError> {
        // biz:226-228: settings validated before the duplicate probe. (The
        // biz:222-224 nil-settings check is unreachable — non-null input.)
        let core_settings = protection_settings_input_to_core(input.settings);
        validate_rule_settings(&input.pattern, &core_settings)?;

        let ctx = boot_request_context();
        // Retained for the duplicate-name error (repo `NameConflict` maps to
        // Go `xerrors.DuplicateNameError("prompt protection rule", …)`).
        let name = input.name.clone();

        let repo_input = RepoCreateRuleInput {
            name: input.name,
            description: input.description,
            pattern: input.pattern,
            settings: protection_settings_core_to_json(&core_settings),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        // `status` is never set on create (ent `SkipMutationCreateInput`) —
        // the column default `disabled` applies inside the repo.
        let row = self
            .rule_repo
            .create_protection_rule_unchecked(&ctx, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => PromptServiceError::DuplicateRuleName(name),
                other => PromptServiceError::CreateRule(other.to_string()),
            })?;
        Ok(rule_row_to_gql(row))
    }

    async fn update_prompt_protection_rule(
        &self,
        id: &str,
        input: UpdatePromptProtectionRuleInput,
    ) -> Result<PromptProtectionRule, PromptServiceError> {
        let ctx = boot_request_context();
        // biz:254-257: `Get` the current row first; failures (including not
        // found) surface as "failed to query prompt protection rule: …".
        let db_id = rule_db_id(id).ok_or_else(|| {
            PromptServiceError::RuleQuery(RepoError::NotFound("prompt protection rule").to_string())
        })?;
        let current = self
            .rule_repo
            .find_protection_rule_unchecked(&ctx, &db_id)
            .await
            .map_err(|e| PromptServiceError::RuleQuery(e.to_string()))?
            .ok_or_else(|| {
                PromptServiceError::RuleQuery(
                    RepoError::NotFound("prompt protection rule").to_string(),
                )
            })?;

        // biz:259-266: validate the *effective* pattern/settings — the input
        // value when supplied, the stored one otherwise (lo.FromPtrOr).
        let effective_pattern = input
            .pattern
            .clone()
            .unwrap_or_else(|| current.pattern.clone());
        let input_core_settings = input.settings.map(protection_settings_input_to_core);
        let effective_settings = match &input_core_settings {
            Some(s) => s.clone(),
            None => serde_json::from_value(current.settings).unwrap_or_default(),
        };
        validate_rule_settings(&effective_pattern, &effective_settings)?;

        // Name (if any) retained for the duplicate-name error.
        let name = input.name.clone();

        // Partial patch mirrors Go `SetInput` (only non-nil fields written).
        let repo_input = RepoUpdateRuleInput {
            name: input.name,
            description: input.description,
            pattern: input.pattern,
            status: input.status.map(|s| rule_status_to_wire(s).to_string()),
            settings: input_core_settings
                .as_ref()
                .map(protection_settings_core_to_json),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let row = self
            .rule_repo
            .update_protection_rule_unchecked(&ctx, &db_id, repo_input)
            .await
            .map_err(|e| match e {
                // Go has no update-time pre-probe; the unique-index violation
                // surfaces through the save error. The typed repo conflict is
                // mapped to the crate's duplicate-name wording, matching the
                // sibling Prompt adapter's convention.
                RepoError::NameConflict => {
                    PromptServiceError::DuplicateRuleName(name.unwrap_or_default())
                }
                other => PromptServiceError::UpdateRule(other.to_string()),
            })?;
        Ok(rule_row_to_gql(row))
    }

    async fn delete_prompt_protection_rule(&self, id: &str) -> Result<(), PromptServiceError> {
        let ctx = boot_request_context();
        let db_id = rule_db_id(id).ok_or_else(|| {
            PromptServiceError::DeleteRule(
                RepoError::NotFound("prompt protection rule").to_string(),
            )
        })?;
        // Soft delete (ent SoftDeleteMixin intercepts Go's DeleteOneID);
        // failures wrap as "failed to delete prompt protection rule: …"
        // (biz:283-291).
        self.rule_repo
            .soft_delete_protection_rule_unchecked(&ctx, &db_id)
            .await
            .map_err(|e| PromptServiceError::DeleteRule(e.to_string()))?;
        Ok(())
    }

    async fn update_prompt_protection_rule_status(
        &self,
        id: &str,
        status: PromptProtectionRuleStatus,
    ) -> Result<PromptProtectionRule, PromptServiceError> {
        let ctx = boot_request_context();
        let db_id = rule_db_id(id).ok_or_else(|| {
            PromptServiceError::UpdateRule(
                RepoError::NotFound("prompt protection rule").to_string(),
            )
        })?;
        // biz:293-304 (`UpdateRuleStatus`): plain SetStatus save; the crate's
        // error surface has no dedicated status variant, so failures use the
        // update wording (same as the in-crate service double).
        let row = self
            .rule_repo
            .set_protection_rule_status_unchecked(
                &ctx,
                &db_id,
                rule_status_to_wire(status),
                chrono::Utc::now().to_rfc3339(),
            )
            .await
            .map_err(|e| PromptServiceError::UpdateRule(e.to_string()))?;
        Ok(rule_row_to_gql(row))
    }

    async fn bulk_delete_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        // biz:306-320: empty ids is a no-op; `IDIn(ids…)` skips missing rows.
        let db_ids: Vec<String> = ids.iter().filter_map(|id| rule_db_id(id)).collect();
        if db_ids.is_empty() {
            return Ok(());
        }
        let ctx = boot_request_context();
        self.rule_repo
            .bulk_delete_protection_rules_unchecked(&ctx, &db_ids)
            .await
            .map_err(|e| PromptServiceError::DeleteRule(e.to_string()))?;
        Ok(())
    }

    async fn bulk_disable_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        self.bulk_set_rule_status(ids, RULE_STATUS_DISABLED).await
    }

    async fn bulk_enable_prompt_protection_rules(
        &self,
        ids: Vec<String>,
    ) -> Result<(), PromptServiceError> {
        self.bulk_set_rule_status(ids, RULE_STATUS_ENABLED).await
    }

    async fn preview_prompt_protection_rule(
        &self,
        input: PromptProtectionRulePreviewInput,
    ) -> Result<PromptProtectionRulePreviewResult, PromptServiceError> {
        // biz/prompt_protection_preview.go:21-52 — pure regex evaluation, no
        // rows touched. ValidateSettings runs first (it compiles the pattern;
        // the compiled regex is reused for matching, mirroring Go's compile
        // cache).
        let core_settings = protection_settings_input_to_core(input.settings);
        let re = validate_rule_settings(&input.pattern, &core_settings)?;

        let has_match = re.is_match(&input.test_text);

        // Go: masked replace only when matched AND action == mask; reject
        // returns the literal enum value string; otherwise the text passes
        // through unchanged. `replace_all`'s `$name` capture expansion in the
        // replacement mirrors regexp2's `Replace` substitution syntax.
        let result = if has_match
            && core_settings.action == core_protection::PROMPT_PROTECTION_ACTION_MASK
        {
            let replacement = core_settings.replacement.as_deref().unwrap_or("");
            re.replace_all(&input.test_text, replacement).into_owned()
        } else if has_match
            && core_settings.action == core_protection::PROMPT_PROTECTION_ACTION_REJECT
        {
            core_protection::PROMPT_PROTECTION_ACTION_REJECT.to_owned()
        } else {
            input.test_text
        };

        Ok(PromptProtectionRulePreviewResult { result, has_match })
    }
}

// ---------------------------------------------------------------------------
// Rule `where` predicate evaluation (Query.promptProtectionRules)
// ---------------------------------------------------------------------------

/// Whether a `PromptProtectionRuleRow` satisfies a
/// `PromptProtectionRuleWhereInput` predicate tree. Same evaluation
/// conventions as [`prompt_row_matches_where`]; deferred families: `id`,
/// `createdAt`, `updatedAt` (module doc).
fn rule_row_matches_where(
    row: &PromptProtectionRuleRow,
    w: &PromptProtectionRuleWhereInput,
) -> bool {
    if let Some(inner) = &w.not
        && rule_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| rule_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| rule_row_matches_where(row, c))
    {
        return false;
    }

    // name string family
    if !str_family(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_gt,
        &w.name_gte,
        &w.name_lt,
        &w.name_lte,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
        &w.name_equal_fold,
        &w.name_contains_fold,
    ) {
        return false;
    }

    // description string family
    if !str_family(
        &row.description,
        &w.description,
        &w.description_neq,
        &w.description_in,
        &w.description_not_in,
        &w.description_gt,
        &w.description_gte,
        &w.description_lt,
        &w.description_lte,
        &w.description_contains,
        &w.description_has_prefix,
        &w.description_has_suffix,
        &w.description_equal_fold,
        &w.description_contains_fold,
    ) {
        return false;
    }

    // pattern string family
    if !str_family(
        &row.pattern,
        &w.pattern,
        &w.pattern_neq,
        &w.pattern_in,
        &w.pattern_not_in,
        &w.pattern_gt,
        &w.pattern_gte,
        &w.pattern_lt,
        &w.pattern_lte,
        &w.pattern_contains,
        &w.pattern_has_prefix,
        &w.pattern_has_suffix,
        &w.pattern_equal_fold,
        &w.pattern_contains_fold,
    ) {
        return false;
    }

    // status enum predicates
    if let Some(s) = w.status
        && row.status != rule_status_to_wire(s)
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row.status == rule_status_to_wire(s)
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list.iter().any(|s| row.status == rule_status_to_wire(*s))
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list.iter().any(|s| row.status == rule_status_to_wire(*s))
    {
        return false;
    }

    true
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use conduit_admin_graphql::prompt::{PromptActionInput, PromptProtectionSettingsInput};

    type TestError = Box<dyn std::error::Error>;

    fn prompt_input(name: &str) -> CreatePromptInput {
        CreatePromptInput {
            name: name.into(),
            description: None,
            role: "system".into(),
            content: "postgres prompt".into(),
            status: None,
            order: None,
            settings: PromptSettingsInput {
                action: PromptActionInput {
                    action_type: PromptActionType::Prepend,
                },
                conditions: None,
            },
            project_ids: None,
        }
    }

    fn protection_input(name: &str) -> CreatePromptProtectionRuleInput {
        CreatePromptProtectionRuleInput {
            name: name.into(),
            description: None,
            pattern: "secret-[0-9]+".into(),
            settings: PromptProtectionSettingsInput {
                action: PromptProtectionAction::Mask,
                replacement: Some("[MASKED]".into()),
                scopes: Some(vec![PromptProtectionScope::User]),
            },
        }
    }

    #[tokio::test]
    async fn postgres_prompt_graphql_adapters_round_trip_when_dsn_is_provided()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;

        // The adapter's unscoped compatibility path targets project 1, while
        // a freshly migrated test schema does not seed a project row. Reserve
        // that id explicitly so the later A/B authorization matrix genuinely
        // uses two different projects instead of allocating project 1 twice.
        let default_project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects (name, status, description, profiles) \
             VALUES ($1, 'active', '', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("prompt-default-{}", uuid::Uuid::new_v4().simple()))
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(default_project_id, 1);

        let prompt_adapter = PromptCrudAdapter::new(Arc::new(conduit_db::PgPromptRepo::new(
            database.pool.clone(),
        )));
        let created = prompt_adapter
            .create_prompt(prompt_input("pg-graphql"))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(created.project_id, 1);
        assert_eq!(created.status, PromptStatus::Disabled);
        let prompts = prompt_adapter
            .prompts(PromptConnectionArgs::default())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(prompts.total_count, 1);
        prompt_adapter
            .delete_prompt(created.id.as_str())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            prompt_adapter
                .prompts(PromptConnectionArgs::default())
                .await
                .map_err(|error| error.to_string())?
                .total_count,
            0
        );

        let second_project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects (name, status, description, profiles) \
             VALUES ($1, 'active', '', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("prompt-scope-{}", uuid::Uuid::new_v4().simple()))
        .fetch_one(&database.pool)
        .await?;
        let first_access = AdminAccessScope::Project("1".to_owned());
        let second_access = AdminAccessScope::Project(second_project_id.to_string());
        let first_prompt = prompt_adapter
            .create_prompt_with_access(&first_access, prompt_input("scoped-first"))
            .await
            .map_err(|error| error.to_string())?;
        let second_prompt = prompt_adapter
            .create_prompt_with_access(&second_access, prompt_input("scoped-second"))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(second_prompt.project_id, second_project_id);

        let first_project_prompts = prompt_adapter
            .prompts_with_access(&first_access, PromptConnectionArgs::default())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(first_project_prompts.total_count, 1);
        assert_eq!(
            first_project_prompts
                .edges
                .as_ref()
                .and_then(|edges| edges.first())
                .and_then(Option::as_ref)
                .and_then(|edge| edge.node.as_ref())
                .map(|prompt| prompt.id.as_str()),
            Some(first_prompt.id.as_str())
        );
        assert!(matches!(
            prompt_adapter
                .update_prompt_with_access(
                    &first_access,
                    second_prompt.id.as_str(),
                    UpdatePromptInput {
                        content: Some("must-not-cross-project".to_owned()),
                        ..Default::default()
                    },
                )
                .await,
            Err(PromptServiceError::UpdatePrompt(_))
        ));
        let second_project_prompts = prompt_adapter
            .prompts_with_access(&second_access, PromptConnectionArgs::default())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(second_project_prompts.total_count, 1);
        assert_eq!(
            second_project_prompts
                .edges
                .as_ref()
                .and_then(|edges| edges.first())
                .and_then(Option::as_ref)
                .and_then(|edge| edge.node.as_ref())
                .map(|prompt| prompt.content.as_str()),
            Some("postgres prompt")
        );

        let rule_adapter = PromptProtectionRuleAdapter::new(Arc::new(
            conduit_db::PgPromptProtectionRuleRepo::new(database.pool.clone()),
        ));
        let rule = rule_adapter
            .create_prompt_protection_rule(protection_input("pg-protection"))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(rule.status, PromptProtectionRuleStatus::Disabled);
        let rules = rule_adapter
            .prompt_protection_rules(PromptProtectionRuleConnectionArgs::default())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(rules.total_count, 1);

        database.cleanup().await?;
        Ok(())
    }
}

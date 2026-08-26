use std::{
    cmp::Ordering,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_core::objects::prompt::{
    PromptActivationCondition, PromptActivationConditionComposite, PromptSettings, action_type,
    activation_condition_type,
};
use conduit_db::RequestContext;
use conduit_llm::{ChatMessage, MessageContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use conduit_core::objects::prompt::{
    PromptAction as CorePromptAction, PromptActivationCondition as CorePromptActivationCondition,
    PromptActivationConditionComposite as CorePromptActivationConditionComposite,
    PromptSettings as CorePromptSettings,
};

/// X-regexp parity: characters that force regex-mode evaluation in
/// `conduit/internal/pkg/xregexp/match.go` (`containsRegexChars`).
const REGEX_CHARS: &[char] = &[
    '*', '?', '+', '[', ']', '{', '}', '(', ')', '^', '$', '.', '|', '\\',
];

pub type PromptServiceResult<T> = Result<T, PromptServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PromptServiceError {
    #[error("prompt persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSort {
    OrderAsc,
    OrderDesc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptListQuery {
    pub project_id: String,
    pub status: Option<PromptStatus>,
    pub order: Option<i32>,
    pub sort: PromptSort,
}

impl PromptListQuery {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            status: None,
            order: None,
            sort: PromptSort::OrderAsc,
        }
    }

    pub fn with_status(mut self, status: PromptStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_order(mut self, order: i32) -> Self {
        self.order = Some(order);
        self
    }

    pub fn with_sort(mut self, sort: PromptSort) -> Self {
        self.sort = sort;
        self
    }
}

/// A prompt entity, mirroring the subset of Go `ent.Prompt` fields needed for
/// the pure-logic matcher (`prompt_matcher.go`) and injector (`ApplyPrompts`).
///
/// The legacy simplified fields (`id`, `name`, `project_id`, `status`, `order`,
/// `content`) are preserved as required for the existing repo/service tests;
/// the new Go-aligned fields (`role`, `settings`, `created_at`) default so the
/// existing constructor and tests continue to work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub status: PromptStatus,
    pub order: i32,
    pub content: String,
    /// Mirrors Go `ent.Prompt.Role` (JSON `"role"`), the message role the
    /// injected prompt should assume (`system` / `developer` / `user`).
    #[serde(default)]
    pub role: String,
    /// Mirrors Go `ent.Prompt.Settings` (JSON `"settings"`). Defaults to an
    /// empty [`PromptSettings`] (action `prepend`, no conditions) which matches
    /// Go zero-value behavior.
    #[serde(default)]
    pub settings: PromptSettings,
    /// Mirrors Go `ent.Prompt.CreatedAt` (JSON `"created_at"`). Used as the
    /// secondary tie-breaker when sorting prompts by `order`, mirroring Go
    /// `sort.SliceStable` in `ApplyPrompts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Prompt {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: impl Into<String>,
        status: PromptStatus,
        order: i32,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            project_id: project_id.into(),
            status,
            order,
            content: content.into(),
            role: String::new(),
            settings: PromptSettings::default(),
            created_at: None,
            extra: BTreeMap::new(),
        }
    }

    /// Builder-style setter for `role`, mirroring Go `ent.Prompt.Role`.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into();
        self
    }

    /// Builder-style setter for `settings`, mirroring Go `ent.Prompt.Settings`.
    pub fn with_settings(mut self, settings: PromptSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Builder-style setter for `created_at`, mirroring Go `ent.Prompt.CreatedAt`.
    pub fn with_created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = Some(created_at);
        self
    }
}

#[async_trait]
pub trait PromptPersistenceRepo: Send + Sync {
    async fn list_prompts(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> PromptServiceResult<Vec<Prompt>>;

    async fn upsert_prompt(
        &self,
        ctx: &RequestContext,
        prompt: Prompt,
    ) -> PromptServiceResult<Prompt>;
}

pub struct PromptService {
    repo: Arc<dyn PromptPersistenceRepo>,
}

impl PromptService {
    pub fn new(repo: Arc<dyn PromptPersistenceRepo>) -> Self {
        Self { repo }
    }

    pub async fn upsert_prompt(
        &self,
        ctx: &RequestContext,
        prompt: Prompt,
    ) -> PromptServiceResult<Prompt> {
        self.repo.upsert_prompt(ctx, prompt).await
    }

    pub async fn list_prompts(
        &self,
        ctx: &RequestContext,
        query: PromptListQuery,
    ) -> PromptServiceResult<Vec<Prompt>> {
        let mut prompts = self.repo.list_prompts(ctx, &query.project_id).await?;

        prompts.retain(|prompt| {
            prompt.project_id == query.project_id
                && query.status.is_none_or(|status| prompt.status == status)
                && query.order.is_none_or(|order| prompt.order == order)
        });

        // Tie-break by id so equal order values stay deterministic across repos.
        prompts.sort_by(|left, right| compare_prompt_order(left, right, query.sort));
        Ok(prompts)
    }
}

fn compare_prompt_order(left: &Prompt, right: &Prompt, sort: PromptSort) -> Ordering {
    let ordering = left
        .order
        .cmp(&right.order)
        .then_with(|| left.id.cmp(&right.id));
    match sort {
        PromptSort::OrderAsc => ordering,
        PromptSort::OrderDesc => ordering.reverse(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryPromptPersistenceRepo {
    inner: Arc<Mutex<BTreeMap<(String, String), Prompt>>>,
}

impl InMemoryPromptPersistenceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(
        &self,
    ) -> PromptServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), Prompt>>> {
        self.inner
            .lock()
            .map_err(|_| PromptServiceError::LockPoisoned)
    }
}

#[async_trait]
impl PromptPersistenceRepo for InMemoryPromptPersistenceRepo {
    async fn list_prompts(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> PromptServiceResult<Vec<Prompt>> {
        Ok(self
            .lock()?
            .values()
            .filter(|prompt| prompt.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn upsert_prompt(
        &self,
        _ctx: &RequestContext,
        prompt: Prompt,
    ) -> PromptServiceResult<Prompt> {
        let mut inner = self.lock()?;
        let key = (prompt.project_id.clone(), prompt.id.clone());
        inner.insert(key, prompt.clone());
        Ok(prompt)
    }
}

// ===========================================================================
// Prompt matcher + injection (pure logic)
//
// Mirrors `conduit/internal/server/biz/prompt_matcher.go`:
//   * `PromptMatcher::match_conditions`           — Go `MatchConditions`
//   * `PromptMatcher::match_composite_condition`  — Go `matchCompositeCondition`
//   * `PromptMatcher::match_condition`            — Go `matchCondition`
//   * `PromptMatcher::filter_matching_prompts`    — Go `FilterMatchingPrompts`
//   * `inject_prompts`                            — Go `ApplyPrompts` (S05)
//
// All functions are pure (no I/O, no globals) and panic-free: they mirror the
// Go control flow exactly, including the default-prepend behavior on an
// unknown action type and the `(order, created_at)` stable sort.
// ===========================================================================

/// X-regexp parity helper mirroring `containsRegexChars` from
/// `conduit/internal/pkg/xregexp/match.go`.
fn contains_regex_chars(pattern: &str) -> bool {
    pattern.chars().any(|c| REGEX_CHARS.contains(&c))
}

/// X-regexp parity helper mirroring `getOrCreatePattern` semantics for the
/// non-regex fast paths (`*` => match-all; no regex chars => exact equality).
/// Returns:
///   * `Some(true)`  when the pattern is `*` (match-all)
///   * `Some(false)` when the pattern has no regex chars (exact equality)
///   * `None`        when the pattern must be evaluated as an anchored regex
fn xregexp_fast_path(pattern: &str) -> Option<bool> {
    if pattern == "*" {
        return Some(true);
    }
    if !contains_regex_chars(pattern) {
        return Some(false);
    }
    None
}

/// X-regexp parity helper mirroring `xregexp.MatchString` for the subset of
/// behavior needed by the prompt matcher (model_id / model_pattern).
///
/// Mirrors the three Go fast paths:
///   1. `pattern == "*"`  => match every input
///   2. pattern has no regex chars => exact string equality
///   3. otherwise => anchored full match `^(?:body)$` compiled with the `regex`
///      crate. The body has a leading `^` / trailing `$` stripped before being
///      re-anchored, exactly like Go `ensureAnchored`.
///
/// Returns `false` (never panics) when the pattern fails to compile, matching
/// Go's `cached.compileErr` branch.
fn xregexp_match_string(pattern: &str, candidate: &str) -> bool {
    match xregexp_fast_path(pattern) {
        Some(true) => true,
        Some(false) => pattern == candidate,
        None => {
            let anchored = ensure_anchored(pattern);
            match regex::Regex::new(&anchored) {
                Ok(re) => re.is_match(candidate),
                Err(_) => false,
            }
        }
    }
}

/// X-regexp parity helper mirroring `ensureAnchored` from
/// `conduit/internal/pkg/xregexp/match.go`. Strips a single leading `^` and
/// trailing `$` from the body, then wraps it as `^(?:body)$`.
///
/// The inline-modifier splitting (`(?i)body`) is intentionally omitted: the
/// matcher only feeds `model_pattern` values here, and the `regex` crate does
/// not support inline `(?i)` scoping the way Go's `regexp2` does, so we keep
/// the parity surface narrow and predictable. Callers that need flags can
/// embed them in the compiled body directly.
fn ensure_anchored(pattern: &str) -> String {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    let body = body.strip_suffix('$').unwrap_or(body);
    format!("^(?:{})$", body)
}

/// State-free prompt matcher mirroring Go `PromptMatcher`
/// (`conduit/internal/server/biz/prompt_matcher.go`).
///
/// All methods are pure and take `&self` only for API parity with Go; the Go
/// type has no fields, so a stateless unit struct is faithful.
#[derive(Debug, Clone, Copy, Default)]
pub struct PromptMatcher;

impl PromptMatcher {
    pub fn new() -> Self {
        Self
    }

    /// Go `MatchPrompt`: returns `false` for a not-applicable prompt and
    /// otherwise delegates to [`Self::match_conditions`] against the prompt's
    /// `settings.conditions`.
    pub fn match_prompt(&self, prompt: Option<&Prompt>, model: &str, api_key_id: i64) -> bool {
        let Some(prompt) = prompt else {
            return false;
        };
        self.match_conditions(&prompt.settings.conditions, model, api_key_id)
    }

    /// Go `MatchConditions`: every composite must be satisfied (AND across
    /// composites). An empty list always matches.
    pub fn match_conditions(
        &self,
        conditions: &[PromptActivationConditionComposite],
        model: &str,
        api_key_id: i64,
    ) -> bool {
        if conditions.is_empty() {
            return true;
        }
        conditions
            .iter()
            .all(|composite| self.match_composite_condition(composite, model, api_key_id))
    }

    /// Go `matchCompositeCondition`: at least one contained condition must be
    /// satisfied (OR within a composite). An empty composite always matches.
    pub fn match_composite_condition(
        &self,
        composite: &PromptActivationConditionComposite,
        model: &str,
        api_key_id: i64,
    ) -> bool {
        if composite.conditions.is_empty() {
            return true;
        }
        composite
            .conditions
            .iter()
            .any(|condition| self.match_condition(condition, model, api_key_id))
    }

    /// Go `matchCondition`: dispatches on `condition.kind` (the Go `Type`
    /// field) to one of the three well-known branches. Unknown kinds return
    /// `false`, matching Go's `default:` arm.
    pub fn match_condition(
        &self,
        condition: &PromptActivationCondition,
        model: &str,
        api_key_id: i64,
    ) -> bool {
        match condition.kind.as_str() {
            activation_condition_type::MODEL_ID => match_model_id(condition, model),
            activation_condition_type::MODEL_PATTERN => match_model_pattern(condition, model),
            activation_condition_type::API_KEY => match_api_key_id(condition, api_key_id),
            _ => false,
        }
    }

    /// Go `FilterMatchingPrompts`: returns the subset of `prompts` whose
    /// activation conditions are satisfied for the given `model` / `api_key_id`.
    pub fn filter_matching_prompts<'a>(
        &self,
        prompts: &'a [Prompt],
        model: &str,
        api_key_id: i64,
    ) -> Vec<&'a Prompt> {
        prompts
            .iter()
            .filter(|p| self.match_prompt(Some(p), model, api_key_id))
            .collect()
    }
}

/// Go `matchModelID`: exact equality between `condition.model_id` and `model`.
/// Returns `false` when `model_id` is absent, mirroring Go's nil-pointer guard.
fn match_model_id(condition: &PromptActivationCondition, model: &str) -> bool {
    condition.model_id.as_deref().is_some_and(|id| id == model)
}

/// Go `matchModelPattern`: anchored full regex match via `xregexp.MatchString`.
/// Returns `false` when `model_pattern` is absent or empty, mirroring Go's
/// nil/empty guard.
fn match_model_pattern(condition: &PromptActivationCondition, model: &str) -> bool {
    condition
        .model_pattern
        .as_deref()
        .is_some_and(|p| !p.is_empty() && xregexp_match_string(p, model))
}

/// Go `matchAPIKeyID`: exact equality between `condition.api_key_id` and the
/// request's `api_key_id`. Returns `false` when absent.
fn match_api_key_id(condition: &PromptActivationCondition, api_key_id: i64) -> bool {
    condition.api_key_id.is_some_and(|id| id == api_key_id)
}

// ---------------------------------------------------------------------------
// S05: prompt injection — Go `ApplyPrompts`
// ---------------------------------------------------------------------------

/// Outcome of [`inject_prompts`] mirroring the *observable* effect of Go's
/// `ApplyPrompts`: the new message list and the per-prompt match reasons that
/// the frontend (S12) needs.
///
/// `matched` records, for each surviving prompt (in sort order), which action
/// bucket it landed in and the role/content it contributed. This is the
/// per-prompt match-reason surface required by RUST-P10-004 S12.
#[derive(Debug, Clone, PartialEq)]
pub struct InjectionResult {
    /// New message list after prepend/append. Always non-empty when the input
    /// `messages` was non-empty.
    pub messages: Vec<ChatMessage>,
    /// Per-prompt reason entries, in the order prompts were applied.
    pub matched: Vec<PromptInjectionReason>,
}

/// Per-prompt match reason for the preview/inject UI (S12). Mirrors the
/// information the Go `ApplyPrompts` loop derives from each prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptInjectionReason {
    /// ID of the prompt this reason describes. Optional to mirror Go, which
    /// synthesizes prompts without an ID in some tests.
    pub prompt_id: Option<String>,
    /// Role that the prompt contributed (e.g. `system`, `developer`, `user`).
    pub role: String,
    /// Content that the prompt contributed.
    pub content: String,
    /// Bucket the prompt landed in: `"prepend"`, `"append"`, or `"prepend"`
    /// again when the action type was unknown (Go default branch).
    pub action: String,
}

/// Pure-logic prompt injection. Mirrors Go
/// `PromptMatcher.ApplyPrompts(request, prompts)` from
/// `conduit/internal/server/biz/prompt_matcher.go` lines 116-160.
///
/// Contract (quoted from Go):
///   * "Prompts with action type `"prepend"` are added before existing
///     messages."
///   * "Prompts with action type `"append"` are added after existing messages."
///   * "Prompts are sorted by their `Order` field (ascending), with `CreatedAt`
///     as tiebreaker."
///   * The `default:` arm of the action switch falls back to **prepend**.
///
/// Because this is a pure function, it does *not* mutate the incoming
/// `messages`; it returns a new [`Vec<ChatMessage>`] (wrapped in
/// [`InjectionResult`]) built as `prepend ++ messages ++ append`, exactly like
/// the Go `newMessages` slice.
///
/// `created_at` tie-breaking: Go uses `CreatedAt.Before(...)`. We mirror that
/// with `DateTime::cmp`; prompts without a `created_at` sort as
/// `DateTime::MIN` (oldest) so the ordering is total and deterministic.
pub fn inject_prompts(messages: &[ChatMessage], prompts: &[Prompt]) -> InjectionResult {
    if prompts.is_empty() {
        return InjectionResult {
            messages: messages.to_vec(),
            matched: Vec::new(),
        };
    }

    // Stable sort: `(order asc, created_at asc)` — Go's `sort.SliceStable`.
    // We clone into a Vec<Prompt> so we own the ordering without borrowing
    // prompts mutably (the Go code sorts its `prompts []*ent.Prompt` in place;
    // we keep it pure by cloning).
    let mut sorted: Vec<Prompt> = prompts.to_vec();
    sorted.sort_by(|a, b| {
        let by_order = a.order.cmp(&b.order);
        if by_order != Ordering::Equal {
            return by_order;
        }
        let a_ts = a.created_at.unwrap_or_else(min_time);
        let b_ts = b.created_at.unwrap_or_else(min_time);
        a_ts.cmp(&b_ts)
    });

    let mut prepend_messages: Vec<ChatMessage> = Vec::new();
    let mut append_messages: Vec<ChatMessage> = Vec::new();
    let mut matched: Vec<PromptInjectionReason> = Vec::new();

    for prompt in &sorted {
        let msg = prompt_to_message(prompt);
        let action_kind = prompt.settings.action.kind.clone();
        let bucket = match action_kind.as_str() {
            action_type::APPEND => "append",
            // Go's `default:` arm treats unknown / empty action as prepend.
            _ => "prepend",
        };
        if matches!(bucket, "append") {
            append_messages.push(msg);
        } else {
            prepend_messages.push(msg);
        }
        matched.push(PromptInjectionReason {
            prompt_id: Some(prompt.id.clone()),
            role: prompt.role.clone(),
            content: prompt.content.clone(),
            action: bucket.to_string(),
        });
    }

    let mut new_messages =
        Vec::with_capacity(prepend_messages.len() + messages.len() + append_messages.len());
    new_messages.extend(prepend_messages);
    new_messages.extend(messages.iter().cloned());
    new_messages.extend(append_messages);

    InjectionResult {
        messages: new_messages,
        matched,
    }
}

/// Builds a [`ChatMessage`] from a [`Prompt`] exactly like Go's
/// `ApplyPrompts` loop: `Message{ Role: prompt.Role, Content: MessageContent{
/// Content: &prompt.Content } }`. In Rust the single-text form is
/// `MessageContent::Text`.
fn prompt_to_message(prompt: &Prompt) -> ChatMessage {
    ChatMessage {
        role: prompt.role.clone(),
        content: Some(MessageContent::Text(prompt.content.clone())),
        name: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        extra: BTreeMap::new(),
    }
}

/// Smallest representable `DateTime<Utc>`; used as the sort key for prompts
/// without a `created_at` so the ordering is total (Go zero-time = oldest).
fn min_time() -> DateTime<Utc> {
    DateTime::<Utc>::MIN_UTC
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn list_prompts_filters_status_order_and_sorts() -> PromptServiceResult<()> {
        let repo = Arc::new(InMemoryPromptPersistenceRepo::new());
        let service = PromptService::new(repo);
        let ctx = ctx();

        for prompt in [
            Prompt::new(
                "prompt-2",
                "Two",
                "project-a",
                PromptStatus::Active,
                20,
                "two",
            ),
            Prompt::new(
                "prompt-1",
                "One",
                "project-a",
                PromptStatus::Active,
                10,
                "one",
            ),
            Prompt::new(
                "prompt-3",
                "Three",
                "project-a",
                PromptStatus::Draft,
                10,
                "three",
            ),
            Prompt::new(
                "prompt-4",
                "Four",
                "project-b",
                PromptStatus::Active,
                10,
                "four",
            ),
        ] {
            service.upsert_prompt(&ctx, prompt).await?;
        }

        let prompts = service
            .list_prompts(
                &ctx,
                PromptListQuery::new("project-a")
                    .with_status(PromptStatus::Active)
                    .with_sort(PromptSort::OrderAsc),
            )
            .await?;
        let order_match = service
            .list_prompts(&ctx, PromptListQuery::new("project-a").with_order(10))
            .await?;

        assert_eq!(
            prompts
                .iter()
                .map(|prompt| prompt.id.as_str())
                .collect::<Vec<_>>(),
            vec!["prompt-1", "prompt-2"]
        );
        assert_eq!(
            order_match
                .iter()
                .map(|prompt| prompt.id.as_str())
                .collect::<Vec<_>>(),
            vec!["prompt-1", "prompt-3"]
        );
        Ok(())
    }

    // =======================================================================
    // PromptMatcher tests — mirror `TestPromptMatcher_MatchConditions`,
    // `TestPromptMatcher_MatchPrompt`, `TestPromptMatcher_FilterMatchingPrompts`
    // from `conduit/internal/server/biz/prompt_matcher_test.go`.
    // =======================================================================

    use conduit_core::objects::prompt::{
        PromptAction, PromptActivationCondition, PromptActivationConditionComposite,
        PromptSettings, action_type, activation_condition_type,
    };
    use conduit_llm::{ChatMessage, MessageContent};

    fn cond(kind: &str) -> PromptActivationCondition {
        PromptActivationCondition {
            kind: kind.to_string(),
            ..Default::default()
        }
    }

    fn composite(conds: Vec<PromptActivationCondition>) -> PromptActivationConditionComposite {
        PromptActivationConditionComposite { conditions: conds }
    }

    fn matcher_case(
        conditions: Vec<PromptActivationConditionComposite>,
        model: &str,
        api_key_id: i64,
    ) -> bool {
        PromptMatcher::new().match_conditions(&conditions, model, api_key_id)
    }

    #[test]
    fn match_conditions_empty_composites_always_match() {
        assert!(matcher_case(Vec::new(), "gpt-4", 0));
    }

    #[test]
    fn match_conditions_model_id_exact_match() {
        let mut c = cond(activation_condition_type::MODEL_ID);
        c.model_id = Some("gpt-4".to_string());
        assert!(matcher_case(vec![composite(vec![c])], "gpt-4", 0));
    }

    #[test]
    fn match_conditions_model_id_mismatch() {
        let mut c = cond(activation_condition_type::MODEL_ID);
        c.model_id = Some("gpt-4".to_string());
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-3.5-turbo", 0));
    }

    #[test]
    fn match_conditions_model_pattern_match() {
        let mut c = cond(activation_condition_type::MODEL_PATTERN);
        c.model_pattern = Some("gpt-4.*".to_string());
        assert!(matcher_case(vec![composite(vec![c])], "gpt-4-turbo", 0));
    }

    #[test]
    fn match_conditions_model_pattern_mismatch() {
        let mut c = cond(activation_condition_type::MODEL_PATTERN);
        c.model_pattern = Some("gpt-4.*".to_string());
        assert!(!matcher_case(vec![composite(vec![c])], "claude-3-opus", 0));
    }

    #[test]
    fn match_conditions_api_key_match() {
        let mut c = cond(activation_condition_type::API_KEY);
        c.api_key_id = Some(1);
        assert!(matcher_case(vec![composite(vec![c])], "gpt-4", 1));
    }

    #[test]
    fn match_conditions_api_key_mismatch() {
        let mut c = cond(activation_condition_type::API_KEY);
        c.api_key_id = Some(1);
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-4", 2));
    }

    #[test]
    fn match_conditions_composite_or_one_matches() {
        let mut a = cond(activation_condition_type::MODEL_ID);
        a.model_id = Some("gpt-4".to_string());
        let mut b = cond(activation_condition_type::MODEL_ID);
        b.model_id = Some("gpt-3.5-turbo".to_string());
        assert!(matcher_case(
            vec![composite(vec![a, b])],
            "gpt-3.5-turbo",
            0
        ));
    }

    #[test]
    fn match_conditions_composite_or_none_matches() {
        let mut a = cond(activation_condition_type::MODEL_ID);
        a.model_id = Some("gpt-4".to_string());
        let mut b = cond(activation_condition_type::MODEL_ID);
        b.model_id = Some("gpt-3.5-turbo".to_string());
        assert!(!matcher_case(
            vec![composite(vec![a, b])],
            "claude-3-opus",
            0
        ));
    }

    #[test]
    fn match_conditions_multiple_composites_and_all_match() {
        let mut a = cond(activation_condition_type::MODEL_PATTERN);
        a.model_pattern = Some("gpt-.*".to_string());
        let mut b = cond(activation_condition_type::MODEL_PATTERN);
        b.model_pattern = Some(".*-4".to_string());
        assert!(matcher_case(
            vec![composite(vec![a]), composite(vec![b])],
            "gpt-4",
            0
        ));
    }

    #[test]
    fn match_conditions_multiple_composites_and_one_fails() {
        let mut a = cond(activation_condition_type::MODEL_PATTERN);
        a.model_pattern = Some("gpt-.*".to_string());
        let mut b = cond(activation_condition_type::MODEL_PATTERN);
        b.model_pattern = Some(".*-turbo".to_string());
        assert!(!matcher_case(
            vec![composite(vec![a]), composite(vec![b])],
            "gpt-4",
            0
        ));
    }

    #[test]
    fn match_conditions_nil_model_id_does_not_match() {
        let c = cond(activation_condition_type::MODEL_ID);
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-4", 0));
    }

    #[test]
    fn match_conditions_empty_model_pattern_does_not_match() {
        let mut c = cond(activation_condition_type::MODEL_PATTERN);
        c.model_pattern = Some(String::new());
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-4", 0));
    }

    #[test]
    fn match_conditions_nil_api_key_id_does_not_match() {
        let c = cond(activation_condition_type::API_KEY);
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-4", 1));
    }

    #[test]
    fn match_conditions_unknown_condition_type_does_not_match() {
        let c = cond("unknown_type");
        assert!(!matcher_case(vec![composite(vec![c])], "gpt-4", 0));
    }

    /// Mirrors Go `TestPromptMatcher_MatchPrompt` "nil prompt should not match"
    /// (prompt_matcher_test.go L253-259): a nil prompt must return false.
    #[test]
    fn match_prompt_nil_prompt_does_not_match() {
        assert!(!PromptMatcher::new().match_prompt(None, "gpt-4", 0));
    }

    #[test]
    fn match_prompt_no_conditions_matches() {
        let prompt = Prompt::new("p1", "p", "proj", PromptStatus::Active, 0, "content")
            .with_role("system")
            .with_settings(PromptSettings {
                action: PromptAction {
                    kind: action_type::PREPEND.to_string(),
                },
                conditions: Vec::new(),
            });
        assert!(PromptMatcher::new().match_prompt(Some(&prompt), "gpt-4", 0));
    }

    #[test]
    fn match_prompt_with_matching_condition_matches() {
        let mut c = cond(activation_condition_type::MODEL_ID);
        c.model_id = Some("gpt-4".to_string());
        let prompt = Prompt::new("p2", "p", "proj", PromptStatus::Active, 0, "content")
            .with_role("system")
            .with_settings(PromptSettings {
                action: PromptAction {
                    kind: action_type::PREPEND.to_string(),
                },
                conditions: vec![composite(vec![c])],
            });
        assert!(PromptMatcher::new().match_prompt(Some(&prompt), "gpt-4", 0));
    }

    #[test]
    fn match_prompt_with_non_matching_condition_does_not_match() {
        let mut c = cond(activation_condition_type::MODEL_ID);
        c.model_id = Some("claude-3-opus".to_string());
        let prompt = Prompt::new("p3", "p", "proj", PromptStatus::Active, 0, "content")
            .with_role("system")
            .with_settings(PromptSettings {
                action: PromptAction {
                    kind: action_type::PREPEND.to_string(),
                },
                conditions: vec![composite(vec![c])],
            });
        assert!(!PromptMatcher::new().match_prompt(Some(&prompt), "gpt-4", 0));
    }

    #[test]
    fn filter_matching_prompts_keeps_only_matching() {
        let make_prompt = |id: &str, pattern: Option<&str>| {
            let mut settings = PromptSettings {
                action: PromptAction {
                    kind: action_type::PREPEND.to_string(),
                },
                conditions: Vec::new(),
            };
            if let Some(p) = pattern {
                let mut c = cond(activation_condition_type::MODEL_PATTERN);
                c.model_pattern = Some(p.to_string());
                settings.conditions.push(composite(vec![c]));
            }
            Prompt::new(id, id, "proj", PromptStatus::Active, 0, "content").with_settings(settings)
        };

        let prompts = vec![
            make_prompt("p1", None),
            make_prompt("p2", Some("gpt-.*")),
            make_prompt("p3", Some("claude-.*")),
        ];

        let matcher = PromptMatcher::new();
        let gpt = matcher.filter_matching_prompts(&prompts, "gpt-4", 0);
        let claude = matcher.filter_matching_prompts(&prompts, "claude-3-opus", 0);
        let unknown = matcher.filter_matching_prompts(&prompts, "unknown-model", 0);

        assert_eq!(
            gpt.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["p1", "p2"]
        );
        assert_eq!(
            claude.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["p1", "p3"]
        );
        assert_eq!(
            unknown.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["p1"]
        );
    }

    // =======================================================================
    // inject_prompts tests — mirror `TestPromptMatcher_ApplyPrompts` from
    // `conduit/internal/server/biz/prompt_matcher_test.go`.
    // =======================================================================

    fn settings(action_type_str: &str) -> PromptSettings {
        PromptSettings {
            action: PromptAction {
                kind: action_type_str.to_string(),
            },
            conditions: Vec::new(),
        }
    }

    fn user_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(content.to_string())),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    fn text_of(msg: &ChatMessage) -> Option<&str> {
        match msg.content.as_ref()? {
            MessageContent::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[test]
    fn inject_no_prompts_returns_original_messages() {
        let messages = vec![user_message("Hello, how are you?")];
        let result = inject_prompts(&messages, &[]);
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.matched.len(), 0);
        assert_eq!(result.messages[0].role, "user");
    }

    #[test]
    fn inject_prepend_single_prompt_lands_before_user() {
        let messages = vec![user_message("Hello, how are you?")];
        let prompt = Prompt::new(
            "p1",
            "p",
            "proj",
            PromptStatus::Active,
            0,
            "You are a helpful assistant.",
        )
        .with_role("system")
        .with_settings(settings(action_type::PREPEND));
        let result = inject_prompts(&messages, &[prompt]);

        assert_eq!(result.messages.len(), 2);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user"]
        );
        assert_eq!(
            text_of(&result.messages[0]),
            Some("You are a helpful assistant.")
        );
        assert_eq!(result.matched.len(), 1);
        assert_eq!(result.matched[0].action, "prepend");
    }

    #[test]
    fn inject_append_single_prompt_lands_after_user() {
        let messages = vec![user_message("Hello, how are you?")];
        let prompt = Prompt::new(
            "p1",
            "p",
            "proj",
            PromptStatus::Active,
            0,
            "Remember to be concise.",
        )
        .with_role("system")
        .with_settings(settings(action_type::APPEND));
        let result = inject_prompts(&messages, &[prompt]);

        assert_eq!(result.messages.len(), 2);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "system"]
        );
        assert_eq!(
            text_of(&result.messages[1]),
            Some("Remember to be concise.")
        );
        assert_eq!(result.matched[0].action, "append");
    }

    #[test]
    fn inject_prepend_and_append_split_correctly() {
        let messages = vec![user_message("Hello, how are you?")];
        let pre = Prompt::new(
            "p1",
            "p",
            "proj",
            PromptStatus::Active,
            0,
            "You are a helpful assistant.",
        )
        .with_role("system")
        .with_settings(settings(action_type::PREPEND));
        let post = Prompt::new("p2", "p", "proj", PromptStatus::Active, 0, "Be concise.")
            .with_role("system")
            .with_settings(settings(action_type::APPEND));
        let result = inject_prompts(&messages, &[pre, post]);

        assert_eq!(result.messages.len(), 3);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user", "system"]
        );
        assert_eq!(
            text_of(&result.messages[0]),
            Some("You are a helpful assistant.")
        );
        assert_eq!(text_of(&result.messages[2]), Some("Be concise."));
    }

    #[test]
    fn inject_multiple_prepends_maintain_order_after_sort() {
        let messages = vec![user_message("Hello, how are you?")];
        // Deliberately pass them in the "wrong" order; the function sorts by
        // `order` ascending. Without a `created_at` both share MIN_UTC, so the
        // tie-break is stable (insertion order preserved).
        let second = Prompt::new(
            "p1",
            "p",
            "proj",
            PromptStatus::Active,
            10,
            "First prepend (order=10).",
        )
        .with_role("system")
        .with_settings(settings(action_type::PREPEND));
        let first = Prompt::new(
            "p2",
            "p",
            "proj",
            PromptStatus::Active,
            5,
            "Earlier prepend (order=5).",
        )
        .with_role("system")
        .with_settings(settings(action_type::PREPEND));
        let result = inject_prompts(&messages, &[second, first]);

        assert_eq!(result.messages.len(), 3);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "system", "user"]
        );
        // order=5 must come first.
        assert_eq!(
            text_of(&result.messages[0]),
            Some("Earlier prepend (order=5).")
        );
        assert_eq!(
            text_of(&result.messages[1]),
            Some("First prepend (order=10).")
        );
    }

    /// Mirrors Go `TestPromptMatcher_ApplyPrompts` "multiple prepends maintain
    /// order" (prompt_matcher_test.go L471-492). Both prompts have the same
    /// `order` (Go zero value 0) and no `created_at`, so Go's
    /// `sort.SliceStable` preserves insertion order: "First prepend." before
    /// "Second prepend.".
    #[test]
    fn inject_multiple_prepends_with_equal_order_preserve_input_order() {
        let messages = vec![user_message("Hello, how are you?")];
        let first = Prompt::new("p1", "p", "proj", PromptStatus::Active, 0, "First prepend.")
            .with_role("system")
            .with_settings(settings(action_type::PREPEND));
        let second = Prompt::new(
            "p2",
            "p",
            "proj",
            PromptStatus::Active,
            0,
            "Second prepend.",
        )
        .with_role("system")
        .with_settings(settings(action_type::PREPEND));
        let result = inject_prompts(&messages, &[first, second]);

        assert_eq!(result.messages.len(), 3);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "system", "user"]
        );
        // Stable sort preserves insertion order for equal order keys.
        assert_eq!(text_of(&result.messages[0]), Some("First prepend."));
        assert_eq!(text_of(&result.messages[1]), Some("Second prepend."));
    }

    #[test]
    fn inject_default_action_is_prepend() {
        let messages = vec![user_message("Hello, how are you?")];
        let prompt = Prompt::new(
            "p1",
            "p",
            "proj",
            PromptStatus::Active,
            0,
            "Default action prompt.",
        )
        .with_role("system")
        .with_settings(settings(""));
        let result = inject_prompts(&messages, &[prompt]);

        assert_eq!(result.messages.len(), 2);
        assert_eq!(
            result
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user"]
        );
        assert_eq!(text_of(&result.messages[0]), Some("Default action prompt."));
        assert_eq!(result.matched[0].action, "prepend");
    }

    #[test]
    fn inject_created_at_is_used_as_tiebreaker_when_order_is_equal() {
        // Two prompts with the same `order` but different `created_at`:
        // the older one must sort first (Go `CreatedAt.Before`).
        let messages = vec![user_message("hi")];
        let older = Prompt::new("old", "p", "proj", PromptStatus::Active, 5, "older")
            .with_role("system")
            .with_settings(settings(action_type::PREPEND))
            .with_created_at(DateTime::<Utc>::MIN_UTC);
        let newer = Prompt::new("new", "p", "proj", PromptStatus::Active, 5, "newer")
            .with_role("system")
            .with_settings(settings(action_type::PREPEND))
            .with_created_at(Utc::now());
        let result = inject_prompts(&messages, &[newer.clone(), older.clone()]);

        // Prepend bucket preserves sort order: older first, then newer, then user.
        assert_eq!(
            result
                .matched
                .iter()
                .map(|r| r.prompt_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("old"), Some("new")]
        );
    }

    // =======================================================================
    // xregexp parity sanity tests — these do NOT exist as standalone Go tests
    // but the Go `MatchString` fast paths are exercised indirectly by
    // `TestPromptMatcher_MatchConditions` (model_pattern branch). We pin them
    // explicitly to catch regressions in the parity port.
    // =======================================================================

    #[test]
    fn xregexp_match_star_matches_anything() {
        assert!(xregexp_match_string("*", "gpt-4"));
        assert!(xregexp_match_string("*", ""));
    }

    #[test]
    fn xregexp_match_no_regex_chars_is_exact_equality() {
        assert!(xregexp_match_string("gpt-4", "gpt-4"));
        assert!(!xregexp_match_string("gpt-4", "gpt-4-turbo"));
    }

    #[test]
    fn xregexp_match_regex_pattern_is_anchored_full_match() {
        // Go ensureAnchored strips ^ and $ then re-anchors. `gpt-4.*` becomes
        // `^(?:gpt-4.*)$` and must match the whole candidate.
        assert!(xregexp_match_string("gpt-4.*", "gpt-4-turbo"));
        assert!(!xregexp_match_string("gpt-4.*", "claude-3-opus"));
    }

    #[test]
    fn xregexp_match_unanchored_body_is_still_treated_as_full_match() {
        // Without explicit anchors, Go wraps with ^(?:body)$ — so `gpt` alone
        // should NOT match `gpt-4` (full-match parity).
        assert!(!xregexp_match_string("gpt", "gpt-4"));
        assert!(xregexp_match_string("gpt.*", "gpt-4"));
    }

    #[test]
    fn xregexp_match_invalid_pattern_returns_false() {
        assert!(!xregexp_match_string("(unclosed", "anything"));
    }
}

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use conduit_core::objects::prompt_protection::{
    PROMPT_PROTECTION_ACTION_MASK, PROMPT_PROTECTION_ACTION_REJECT,
    PROMPT_PROTECTION_SCOPE_ASSISTANT, PROMPT_PROTECTION_SCOPE_DEVELOPER,
    PROMPT_PROTECTION_SCOPE_SYSTEM, PROMPT_PROTECTION_SCOPE_TOOL, PROMPT_PROTECTION_SCOPE_USER,
    PromptProtectionAction, PromptProtectionScope, PromptProtectionSettings,
};
use conduit_db::RequestContext;
use conduit_llm::{ChatMessage, MessageContent};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use conduit_core::objects::prompt_protection::{
    PromptProtectionAction as CorePromptProtectionAction,
    PromptProtectionScope as CorePromptProtectionScope,
    PromptProtectionSettings as CorePromptProtectionSettings,
};

pub type PromptProtectionServiceResult<T> = Result<T, PromptProtectionServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PromptProtectionServiceError {
    #[error("invalid prompt protection pattern for {rule_id}: {message}")]
    InvalidPattern { rule_id: String, message: String },
    #[error("prompt protection persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptRuleStatus {
    Enabled,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptRuleAction {
    Allow,
    Block,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptRule {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub status: PromptRuleStatus,
    pub order: i32,
    pub pattern: String,
    pub action: PromptRuleAction,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PromptRule {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: impl Into<String>,
        status: PromptRuleStatus,
        order: i32,
        pattern: impl Into<String>,
        action: PromptRuleAction,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            project_id: project_id.into(),
            status,
            order,
            pattern: pattern.into(),
            action,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptProtectionCheck {
    pub project_id: String,
    pub prompt: String,
}

impl PromptProtectionCheck {
    pub fn new(project_id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            prompt: prompt.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptProtectionDecision {
    pub action: PromptRuleAction,
    pub matched_rule_id: Option<String>,
}

impl PromptProtectionDecision {
    pub fn allow() -> Self {
        Self {
            action: PromptRuleAction::Allow,
            matched_rule_id: None,
        }
    }
}

#[async_trait]
pub trait PromptProtectionPersistenceRepo: Send + Sync {
    async fn list_prompt_rules(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> PromptProtectionServiceResult<Vec<PromptRule>>;

    async fn upsert_prompt_rule(
        &self,
        ctx: &RequestContext,
        rule: PromptRule,
    ) -> PromptProtectionServiceResult<PromptRule>;
}

pub struct PromptProtectionService {
    repo: Arc<dyn PromptProtectionPersistenceRepo>,
}

impl PromptProtectionService {
    pub fn new(repo: Arc<dyn PromptProtectionPersistenceRepo>) -> Self {
        Self { repo }
    }

    pub fn validate_rule(rule: &PromptRule) -> PromptProtectionServiceResult<()> {
        compile_rule(rule).map(|_| ())
    }

    pub async fn upsert_rule(
        &self,
        ctx: &RequestContext,
        rule: PromptRule,
    ) -> PromptProtectionServiceResult<PromptRule> {
        Self::validate_rule(&rule)?;
        self.repo.upsert_prompt_rule(ctx, rule).await
    }

    pub async fn check_prompt(
        &self,
        ctx: &RequestContext,
        check: PromptProtectionCheck,
    ) -> PromptProtectionServiceResult<PromptProtectionDecision> {
        let mut rules = self.repo.list_prompt_rules(ctx, &check.project_id).await?;
        rules.retain(|rule| {
            rule.project_id == check.project_id && rule.status == PromptRuleStatus::Enabled
        });
        rules.sort_by(compare_rule_order);

        for rule in rules {
            let pattern = compile_rule(&rule)?;
            if pattern.is_match(&check.prompt) {
                return Ok(PromptProtectionDecision {
                    action: rule.action,
                    matched_rule_id: Some(rule.id),
                });
            }
        }

        Ok(PromptProtectionDecision::allow())
    }
}

fn compile_rule(rule: &PromptRule) -> PromptProtectionServiceResult<Regex> {
    Regex::new(&rule.pattern).map_err(|err| PromptProtectionServiceError::InvalidPattern {
        rule_id: rule.id.clone(),
        message: err.to_string(),
    })
}

fn compare_rule_order(left: &PromptRule, right: &PromptRule) -> Ordering {
    left.order
        .cmp(&right.order)
        .then_with(|| left.id.cmp(&right.id))
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryPromptProtectionPersistenceRepo {
    inner: Arc<Mutex<BTreeMap<(String, String), PromptRule>>>,
}

impl InMemoryPromptProtectionPersistenceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(
        &self,
    ) -> PromptProtectionServiceResult<
        std::sync::MutexGuard<'_, BTreeMap<(String, String), PromptRule>>,
    > {
        self.inner
            .lock()
            .map_err(|_| PromptProtectionServiceError::LockPoisoned)
    }
}

#[async_trait]
impl PromptProtectionPersistenceRepo for InMemoryPromptProtectionPersistenceRepo {
    async fn list_prompt_rules(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> PromptProtectionServiceResult<Vec<PromptRule>> {
        Ok(self
            .lock()?
            .values()
            .filter(|rule| rule.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn upsert_prompt_rule(
        &self,
        _ctx: &RequestContext,
        rule: PromptRule,
    ) -> PromptProtectionServiceResult<PromptRule> {
        let mut inner = self.lock()?;
        let key = (rule.project_id.clone(), rule.id.clone());
        inner.insert(key, rule.clone());
        Ok(rule)
    }
}

// ===========================================================================
// Prompt-protection preview (pure logic) — S07 / S12
//
// Mirrors:
//   * Go `Preview`                — `prompt_protection_preview.go`
//   * Go `ApplyPromptProtectionRules` — `prompt_protection_request.go`
//   * Go `ValidateSettings`       — `prompt_protection_rule.go`
//   * Go `MatchPromptProtectionRule` / `ReplacePromptProtectionRule`
//     — `prompt_protection_rule.go`
//
// All functions here are pure (no I/O, no globals, panic-free). They take the
// request body / rules as values and return the UI preview shape with per-rule
// / per-message match reasons (S12).
// ===========================================================================

/// Well-known valid scopes for the `ValidateSettings` parity check
/// (Go `validScopes` in `prompt_protection_rule.go`).
const VALID_PROTECTION_SCOPES: &[&str] = &[
    PROMPT_PROTECTION_SCOPE_SYSTEM,
    PROMPT_PROTECTION_SCOPE_DEVELOPER,
    PROMPT_PROTECTION_SCOPE_USER,
    PROMPT_PROTECTION_SCOPE_ASSISTANT,
    PROMPT_PROTECTION_SCOPE_TOOL,
];

/// Input for [`preview_pattern`] — the per-rule, per-pattern preview mirroring
/// Go `PromptProtectionPreviewInput` from `prompt_protection_preview.go`.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptProtectionPreviewInput {
    pub pattern: String,
    pub test_text: String,
    pub settings: PromptProtectionSettings,
}

/// Result of [`preview_pattern`] mirroring Go `PromptProtectionPreviewResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptProtectionPreviewResult {
    /// Final preview text. For `mask` action this is the replaced string; for
    /// `reject` action this is the literal `"reject"` marker; when no match,
    /// the original `test_text` is returned.
    pub result: String,
    /// Whether the pattern matched `test_text` at all.
    pub has_match: bool,
}

/// Single-rule match entry in a multi-rule preview ([`ProtectionPreview`]).
/// Mirrors the per-rule information the frontend needs to explain *why* a rule
/// would (or would not) fire (S12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionRuleMatch {
    /// Rule id this entry refers to (the Go `*ent.PromptProtectionRule`).
    pub rule_id: String,
    /// Rule name (Go `Name` field) — the frontend renders this.
    pub rule_name: String,
    /// Pattern evaluated.
    pub pattern: String,
    /// Action that would be taken (`"mask"` / `"reject"`).
    pub action: PromptProtectionAction,
    /// Per-message hit list for this rule.
    pub hits: Vec<ProtectionRuleHit>,
    /// `true` when at least one message matched.
    pub matched: bool,
}

/// Per-message hit recorded by [`preview_protection`]. Describes which message
/// the rule fired on and what the resulting text would be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionRuleHit {
    /// Index of the message in the original request (Go loop variable `i`).
    pub message_index: usize,
    /// Role of the message that matched (`system`/`user`/...).
    pub role: String,
    /// Where inside the message the rule fired: `"content"` for the single-text
    /// branch, `"parts[{i}]"` for a `text`-typed content part.
    pub location: String,
    /// Text **before** the rule was applied (for diff display).
    pub before: String,
    /// Text **after** the rule was applied (masked, or `"reject"` marker).
    pub after: String,
}

/// Outcome of [`preview_protection`] across the whole request. Mirrors the
/// observable shape of Go `ApplyPromptProtectionRules` plus the per-rule
/// reasons the frontend (S12) needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionPreview {
    /// Final message list after masking; `None` when the request would be
    /// rejected (Go sets `Request: nil` on reject).
    pub messages: Option<Vec<ChatMessage>>,
    /// Whether at least one `reject` rule fired.
    pub rejected: bool,
    /// Per-rule match entries, in the order rules were evaluated. Rules that
    /// did not match any message are still included with `matched: false` and
    /// an empty `hits` list — this is the S12 "every rule's reason" surface.
    pub rules: Vec<ProtectionRuleMatch>,
}

/// Validate a protection rule's pattern + settings. Mirrors Go
/// `PromptProtectionRuleService.ValidateSettings` from
/// `prompt_protection_rule.go` lines 92-123.
///
/// Returns the same errors as Go (same order, same messages) so the preview UI
/// can surface them verbatim. Pure: no caching, no I/O.
pub fn validate_protection_settings(
    pattern: &str,
    settings: &PromptProtectionSettings,
) -> Result<(), String> {
    // Go: invalid regex pattern.
    if Regex::new(pattern).is_err() {
        return Err(format!("invalid regex pattern: {}", pattern));
    }

    // Go: invalid action.
    if settings.action != PROMPT_PROTECTION_ACTION_MASK
        && settings.action != PROMPT_PROTECTION_ACTION_REJECT
    {
        return Err(format!("invalid action: {}", settings.action));
    }

    // Go: mask requires replacement.
    if settings.action == PROMPT_PROTECTION_ACTION_MASK
        && settings.replacement.as_ref().is_none_or(|r| r.is_empty())
    {
        return Err("replacement is required for mask action".to_string());
    }

    // Go: invalid scope.
    for scope in &settings.scopes {
        if !VALID_PROTECTION_SCOPES.contains(&scope.as_str()) {
            return Err(format!("invalid scope: {}", scope));
        }
    }

    Ok(())
}

/// Compile a protection pattern. Mirrors Go `getOrCompilePromptProtectionPattern`
/// but without the global cache (pure function). Returns `None` on compile
/// error to mirror Go's `compileErr` branch.
fn compile_protection_pattern(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

/// Match a protection pattern against `content`. Mirrors Go
/// `MatchPromptProtectionRule`. Returns `false` (never panics) when the
/// pattern fails to compile.
pub fn match_protection_pattern(pattern: &str, content: &str) -> bool {
    match compile_protection_pattern(pattern) {
        Some(re) => re.is_match(content),
        None => false,
    }
}

/// Replace all occurrences of `pattern` in `content` with `replacement`.
/// Mirrors Go `ReplacePromptProtectionRule`. Returns the original `content`
/// when the pattern fails to compile (matching Go's error-fallback branch).
pub fn replace_protection_pattern(pattern: &str, content: &str, replacement: &str) -> String {
    match compile_protection_pattern(pattern) {
        Some(re) => re.replace_all(content, replacement).to_string(),
        None => content.to_string(),
    }
}

// ===========================================================================
// S10 — CompiledRules (regex compile-cache, pure)
//
// Mirrors Go `promptProtectionRegexCache` /
// `getOrCompilePromptProtectionPattern` in `prompt_protection_rule.go`
// lines 25-31 and 153-177. The Go cache is a global `xmap` keyed by pattern
// string; the first call compiles, every subsequent call returns the cached
// `*regexp2.Regexp` (or the cached compile error). This pure equivalent
// pre-compiles every (rule_id, pattern) pair **once** at build time and
// reuses the resulting `Regex` for every match/replace call against that
// rule — a single compile per pattern per `CompiledRules` instance, which is
// the parity observable (one compile, not one per message).
//
// Invalid patterns are rejected at build time, mirroring Go's
// `compileErr` branch where the cached entry stores the error and is returned
// for every subsequent lookup.
// ===========================================================================

/// A single rule entry inside a [`CompiledRules`] cache. The `Regex` is
/// compiled exactly once at [`CompiledRules::build`] time and reused for the
/// lifetime of the cache.
#[derive(Debug, Clone)]
pub struct CompiledRuleEntry {
    pub rule_id: String,
    pub pattern: String,
    pub regex: Regex,
    pub settings: PromptProtectionSettings,
}

/// Pure compile-cache for a set of protection rules. Built once via
/// [`CompiledRules::build`]; every match/replace decision after that reuses
/// the cached `Regex` instances — no recompilation. Mirrors Go's
/// `promptProtectionRegexCache` observable behavior (compile-once-per-pattern)
/// without the global mutable state.
#[derive(Debug, Clone, Default)]
pub struct CompiledRules {
    entries: Vec<CompiledRuleEntry>,
}

/// Error returned by [`CompiledRules::build`] when a pattern fails to compile.
/// Mirrors Go's `compileErr` + `err` fields on `promptProtectionPatternCache`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid prompt protection pattern for {rule_id}: {message}")]
pub struct PatternCompileError {
    pub rule_id: String,
    pub pattern: String,
    pub message: String,
}

/// Input row for [`CompiledRules::build`]. Couples a rule identity with the
/// settings that govern its match/replace behavior — the same tuple Go's
/// `*ent.PromptProtectionRule` carries (`ID`, `Pattern`, `Settings`).
#[derive(Debug, Clone)]
pub struct CompiledRuleInput<'a> {
    pub rule_id: String,
    pub pattern: String,
    pub settings: PromptProtectionSettings,
    pub rule: &'a PromptRule,
}

impl CompiledRules {
    /// Build the compile-cache. Each unique pattern is compiled exactly once;
    /// duplicate patterns reuse the first compilation (parity with Go's
    /// pattern-keyed map). Invalid patterns fail the entire build, mirroring
    /// Go's behavior where `getOrCompilePromptProtectionPattern` returns the
    /// cached error and `ValidateSettings` short-circuits on the first bad
    /// pattern.
    pub fn build(inputs: &[CompiledRuleInput<'_>]) -> Result<Self, PatternCompileError> {
        let mut entries: Vec<CompiledRuleEntry> = Vec::with_capacity(inputs.len());

        for input in inputs {
            let regex = match Regex::new(&input.pattern) {
                Ok(re) => re,
                Err(err) => {
                    return Err(PatternCompileError {
                        rule_id: input.rule_id.clone(),
                        pattern: input.pattern.clone(),
                        message: err.to_string(),
                    });
                }
            };
            entries.push(CompiledRuleEntry {
                rule_id: input.rule_id.clone(),
                pattern: input.pattern.clone(),
                regex,
                settings: input.settings.clone(),
            });
        }

        Ok(Self { entries })
    }

    /// Iterate the compiled entries, in insertion order (parity with Go's
    /// in-order rule evaluation in `ApplyPromptProtectionRules`).
    pub fn entries(&self) -> &[CompiledRuleEntry] {
        &self.entries
    }

    /// Look up a compiled entry by rule id. Mirrors a cache hit on the Go
    /// global map by pattern (we key by rule id here because each rule carries
    /// its own settings, but the observable behavior — O(1)-ish lookup of a
    /// pre-compiled regex — is preserved).
    pub fn find(&self, rule_id: &str) -> Option<&CompiledRuleEntry> {
        self.entries.iter().find(|e| e.rule_id == rule_id)
    }
}

// ===========================================================================
// S06 — create-time / update-time rule validation
//
// Mirrors Go `PromptProtectionRuleService.ValidateSettings` (lines 92-123) as
// invoked from Go `CreateRule` (line 226) and `UpdateRule` (line 266). Go
// validates the pattern + settings pair; we additionally validate the rule's
// status field (which Go persists via the ent mutator and which our
// `PromptRule` carries directly). The pure [`validate_protection_rule`]
// helper is what `CreateRule` / `UpdateRule` equivalents should call before
// any persistence.
// ===========================================================================

/// Validate a `PromptRule` for create / update. Mirrors the union of Go
/// `ValidateSettings(pattern, settings)` (pattern compiles, action valid,
/// mask requires replacement, scopes valid) and the implicit Go invariants
/// on `*ent.PromptProtectionRule`:
///   * `Status` is one of the ent enum values (we model it as our
///     [`PromptRuleStatus`], which is already constrained by its enum
///     variants — so the check is `match`-trivial but kept here for parity).
///   * The rule's `pattern` is non-empty (Go `ValidateSettings` would error
///     via `regexp2.Compile("")` returning `nil` err, which compiles to
///     match-everything; we mirror Go's tolerance but flag empty pattern
///     because Go's `CreatePromptProtectionRuleInput` never sends one
///     meaningfully).
///
/// Returns `Err(String)` whose message mirrors Go's error strings verbatim so
/// the API surface can return the same text.
pub fn validate_protection_rule(
    rule: &PromptRule,
    settings: &PromptProtectionSettings,
) -> Result<(), String> {
    // S13 security gate — runs FIRST, before any pattern compilation, so a
    // rule smuggling a forbidden evaluator is refused before we even look at
    // the regex. See [`reject_forbidden_evaluators`]. We collapse the typed
    // `ForbiddenEvaluatorError` into the same `String` error shape the rest
    // of this function uses so the public signature stays `Result<(), String>`
    // (parity with Go `ValidateSettings`'s plain `error`).
    if let Err(forbidden) = reject_forbidden_evaluators(rule) {
        return Err(forbidden.to_string());
    }

    // Go parity: pattern must compile (ValidateSettings line 93-95).
    if Regex::new(&rule.pattern).is_err() {
        return Err(format!("invalid regex pattern: {}", rule.pattern));
    }

    // Status validity — our enum already constrains this, but we surface the
    // same error shape Go's ent layer would if a bad status slipped through.
    match rule.status {
        PromptRuleStatus::Enabled | PromptRuleStatus::Disabled | PromptRuleStatus::Archived => {}
    }

    // Delegate the settings check to the existing parity helper — exact same
    // error strings as Go ValidateSettings.
    validate_protection_settings(&rule.pattern, settings)
}

// ===========================================================================
// RUST-P10-004 S13 — condition-evaluator whitelist (no user-script execution).
//
// # Parity (Go `prompt_protection_rule.go` vs `internal/objects/condition.go`)
//
// The wider Conduit API Go codebase DOES ship a generic expression evaluator:
//   * `internal/objects/condition.go` — `Condition` struct (field / operator /
//     value / nested AND|OR group) compiled to `expr-lang/expr` and executed
//     via `objects.Evaluate(condition, data)`.
//   * Used by: `biz/model.go` (model filters), `orchestrator/candidates*.go`
//     (channel/model candidate gates), `orchestrator/prompt*.go` (system
//     prompt ACTIVATION conditions), and GraphQL `FilterCondition`.
//
// Prompt **protection** (Go `prompt_protection_rule.go` + `_request.go` +
// `_preview.go`) NEVER calls `objects.Evaluate` / `objects.Condition` /
// `expr.Compile` / any script engine. It compiles the rule's `pattern` as a
// `regexp2.Regexp` once (cached), then runs ONLY `MatchString` /
// `Replace` against message text. There is no field/operator/value condition
// tree and no way for a user-authored script to reach execution.
//
// This S13 block encodes that invariant as a typed whitelist gate so that
// any future attempt to sneak a `condition` / `expression` / `script` field
// into a `PromptRule` (via `extra` or otherwise) is rejected at validation
// time — before any code path could dispatch on it. The whitelist is closed
// by default: only [`ProtectionRuleEvaluator::RegexPattern`] is admitted.
// ===========================================================================

/// The closed set of evaluators the prompt-protection engine is allowed to
/// dispatch on. Mirrors the Go invariant that protection rules ONLY ever run
/// a compiled regex against the message text.
///
/// Variants other than [`RegexPattern`] exist solely so that
/// [`classify_protection_rule_kind`] can name the forbidden evaluator in its
/// rejection error — they MUST NOT be admitted by
/// [`is_allowed_protection_evaluator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionRuleEvaluator {
    /// The stock Go path: `pattern` is compiled as a `regexp2` regex and
    /// applied via `MatchString` / `Replace`. No arbitrary code execution.
    RegexPattern,
    /// `expr-lang/expr`-style condition tree (Go `objects.Condition`). Used
    /// elsewhere in Conduit API (model filters, candidates, prompt activation)
    /// but FORBIDDEN in prompt protection.
    Condition,
    /// Raw expression string (`objects.Evaluate` direct input). FORBIDDEN.
    Expression,
    /// Any embedded script (Lua / JS / Tengo / etc.). Conduit API does not ship
    /// a script runtime, but we list it so the gate fails closed if one is
    /// ever introduced. FORBIDDEN.
    Script,
}

impl ProtectionRuleEvaluator {
    /// String spelling used by the wire format / error messages. Stable.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegexPattern => "regex_pattern",
            Self::Condition => "condition",
            Self::Expression => "expression",
            Self::Script => "script",
        }
    }
}

impl std::fmt::Display for ProtectionRuleEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The frozen whitelist of admitted evaluators. Anything not in this set is
/// rejected by [`is_allowed_protection_evaluator`].
///
/// Stock Go prompt protection admits ONLY the regex pattern path, so the
/// whitelist contains exactly one entry. It is exposed as a `const` slice so
/// callers (and tests) can audit the surface without re-deriving it.
pub const PROTECTION_ALLOWED_EVALUATORS: &[ProtectionRuleEvaluator] =
    &[ProtectionRuleEvaluator::RegexPattern];

/// Pure predicate implementing the closed-world whitelist. Returns `true`
/// only for [`ProtectionRuleEvaluator::RegexPattern`] (the stock Go path).
pub fn is_allowed_protection_evaluator(kind: ProtectionRuleEvaluator) -> bool {
    PROTECTION_ALLOWED_EVALUATORS.contains(&kind)
}

/// Wire-format field names that, if present on a `PromptRule`, would signal
/// an attempt to smuggle in a forbidden evaluator. Mirrors the JSON keys
/// Conduit API's generic `objects.Condition` / expression layer would consume.
///
/// Keys are matched case-sensitively against the canonical spellings; we also
/// include a handful of obvious aliases so common typos / camelCase variants
/// are caught the same way. The list is intentionally short and explicit —
/// adding a key here is a security review event, not a routine edit.
const FORBIDDEN_EVALUATOR_FIELDS: &[&str] = &[
    // objects.Condition tree (expr-lang backed).
    "condition",
    "conditions",
    // Raw expr-lang expression string.
    "expression",
    "expr",
    // Generic script / code blob.
    "script",
    "code",
    "eval",
];

/// Outcome of [`classify_protection_rule_kind`]: either the rule is admitted
/// as a stock regex-pattern rule, or it carries a forbidden evaluator field
/// and MUST be rejected before any dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionRuleClassification {
    /// Rule is admissible — the only evaluator it implies is
    /// [`ProtectionRuleEvaluator::RegexPattern`].
    Admitted(ProtectionRuleEvaluator),
    /// Rule carries a forbidden field. The field name and the evaluator it
    /// would imply are surfaced so the API layer can return a precise error.
    Forbidden {
        field: String,
        implies: ProtectionRuleEvaluator,
    },
}

/// Inspect a rule's `extra` JSON for any field that would signal a forbidden
/// evaluator. Pure: no I/O, no globals, panic-free.
///
/// Returns:
/// * `Admitted(RegexPattern)` when no forbidden field is present — the
///   stock-Go behavior.
/// * `Forbidden { field, implies }` when one of
///   [`FORBIDDEN_EVALUATOR_FIELDS`] is present. The `implies` tag is the
///   evaluator kind that field would dispatch on, so callers can produce a
///   precise error message ("rule carries `condition`; prompt protection
///   does not execute condition evaluators — only regex patterns are
///   allowed").
///
/// If multiple forbidden fields are present, the FIRST one encountered in
/// [`FORBIDDEN_EVALUATOR_FIELDS`] order is reported (deterministic for
/// tests).
pub fn classify_protection_rule_kind(rule: &PromptRule) -> ProtectionRuleClassification {
    for forbidden_field in FORBIDDEN_EVALUATOR_FIELDS {
        if rule.extra.contains_key(*forbidden_field) {
            return ProtectionRuleClassification::Forbidden {
                field: (*forbidden_field).to_string(),
                implies: forbidden_field_implies(forbidden_field),
            };
        }
    }
    ProtectionRuleClassification::Admitted(ProtectionRuleEvaluator::RegexPattern)
}

/// Map a forbidden field name to the evaluator kind it implies, for the
/// `Forbidden { implies }` report. Pure lookup.
fn forbidden_field_implies(field: &str) -> ProtectionRuleEvaluator {
    match field {
        "condition" | "conditions" => ProtectionRuleEvaluator::Condition,
        "expression" | "expr" => ProtectionRuleEvaluator::Expression,
        "script" | "code" | "eval" => ProtectionRuleEvaluator::Script,
        _ => ProtectionRuleEvaluator::Script, // fail closed for unknown
    }
}

/// Error returned by [`reject_forbidden_evaluators`] when a rule carries a
/// forbidden evaluator field. Carries the offending field name + implied
/// evaluator so the API layer can surface the precise reason.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "prompt protection rule {rule_id} carries forbidden evaluator field `{field}` \
     (implies `{implies}`); prompt protection only allows regex patterns — \
     user scripts / condition trees are never executed"
)]
pub struct ForbiddenEvaluatorError {
    pub rule_id: String,
    pub field: String,
    pub implies: ProtectionRuleEvaluator,
}

/// Security gate invoked from [`validate_protection_rule`]. Refuses any rule
/// whose `extra` JSON carries a field implying a non-regex evaluator.
///
/// # Why
/// Stock Go prompt protection (`prompt_protection_rule.go`) only ever
/// compiles the `pattern` field as a regex and runs `MatchString` /
/// `Replace`. It does NOT invoke `objects.Evaluate` / `expr.Compile` / any
/// script engine. The wider Go codebase DOES ship `expr-lang/expr`
/// (`internal/objects/condition.go`), so an inattentive future port could
/// accidentally wire that engine into protection rules. This gate makes the
/// invariant enforceable: any rule carrying `condition` / `expression` /
/// `script` / etc. is rejected at create/update time, before any dispatch.
pub fn reject_forbidden_evaluators(rule: &PromptRule) -> Result<(), ForbiddenEvaluatorError> {
    match classify_protection_rule_kind(rule) {
        ProtectionRuleClassification::Admitted(_) => Ok(()),
        ProtectionRuleClassification::Forbidden { field, implies } => {
            Err(ForbiddenEvaluatorError {
                rule_id: rule.id.clone(),
                field,
                implies,
            })
        }
    }
}

// ===========================================================================
// S11 — block-on-match decision (pure)
//
// Mirrors Go `ApplyPromptProtectionRules(req, rules)` from
// `prompt_protection_request.go` lines 24-72 at the single-text level. The
// Go function runs the rule list in order against the request's messages;
// the first `reject` rule that matches **any** message short-circuits with
// `Rejected: true` and `Request: nil` (the caller, `Protect`, then returns
// `ErrPromptProtectionRejected` and the request is **not** forwarded to the
// provider). `mask` rules accumulate text substitutions. If no rule matches
// the request passes through unchanged.
//
// [`decide_protection`] captures that decision for a single text input — the
// unit the orchestrator middleware evaluates per message. The result is a
// [`ProtectionDecision`] the middleware can branch on **before** the provider
// call, which is the S11 contract: "protection runs BEFORE provider call; on
// block, record Request per Go".
// ===========================================================================

/// Outcome of [`decide_protection`] on a single text. Mirrors the three
/// observable branches of Go `ApplyPromptProtectionRules` /
/// `PromptProtectionResult`:
///   * `Allow` — no rule matched; the text passes through verbatim
///     (Go: `MatchedRules: nil`, request forwarded).
///   * `Mask(new_text)` — one or more `mask` rules fired and rewrote the
///     text; the request proceeds with the substituted text (Go:
///     `Request.Messages[i] = updatedMsg`).
///   * `Block { rule_id, reason }` — a `reject` rule matched; the request is
///     rejected before the provider call (Go: `Rejected: true`,
///     `ErrPromptProtectionRejected`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionDecision {
    /// No rule fired — text is unchanged.
    Allow,
    /// At least one `mask` rule fired. Carries the fully-substituted text
    /// after all `mask` rules have been applied in order.
    Mask(String),
    /// A `reject` rule fired. Carries the id of the first rejecting rule and
    /// a human-readable reason. The orchestrator must NOT call the provider.
    Block { rule_id: String, reason: String },
}

impl ProtectionDecision {
    /// Convenience: `true` iff this is [`ProtectionDecision::Block`].
    pub fn is_block(&self) -> bool {
        matches!(self, ProtectionDecision::Block { .. })
    }

    /// Convenience: `true` iff this is [`ProtectionDecision::Allow`].
    pub fn is_allow(&self) -> bool {
        matches!(self, ProtectionDecision::Allow)
    }
}

/// Decide protection for `text` against a pre-compiled rule cache. Pure: no
/// I/O, no globals, panic-free. Rules are evaluated in insertion order (the
/// order [`CompiledRules::build`] received them), mirroring Go's in-order
/// rule loop in `ApplyPromptProtectionRules`.
///
/// Semantics (quoted from Go `prompt_protection_request.go`):
///   * "if `len(rules) == 0` => pass through"  -> `Allow`
///   * "`mask`" rules rewrite the text via `ReplacePromptProtectionRule`
///     (we use `regex.replace_all` on the cached `Regex`).
///   * The first "`reject`" rule that matches short-circuits with
///     `Block { rule_id, reason: "prompt protection rejected request" }`
///     (Go: `ErrPromptProtectionRejected`).
///   * Scopes are **not** consulted here — the caller already filtered the
///     rule list to those whose scopes apply to the message role, mirroring
///     Go's `promptProtectionRuleAppliesToRole` gate that runs **inside**
///     `ApplyPromptProtectionRules` per message. Keeping it out of this
///     helper keeps the decision pure and single-responsibility.
pub fn decide_protection(text: &str, compiled: &CompiledRules) -> ProtectionDecision {
    if text.is_empty() {
        return ProtectionDecision::Allow;
    }

    let mut current = text.to_string();
    let mut masked = false;

    for entry in compiled.entries() {
        if entry.regex.is_match(&current) {
            if entry.settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                // Go: short-circuit on first reject, do NOT forward to provider.
                return ProtectionDecision::Block {
                    rule_id: entry.rule_id.clone(),
                    reason: "prompt protection rejected request".to_string(),
                };
            }
            if entry.settings.action == PROMPT_PROTECTION_ACTION_MASK {
                let replacement = entry.settings.replacement.as_deref().unwrap_or("");
                current = entry.regex.replace_all(&current, replacement).to_string();
                masked = true;
            }
        }
    }

    if masked {
        ProtectionDecision::Mask(current)
    } else {
        ProtectionDecision::Allow
    }
}

/// Apply a single protection pattern to a single text string. Mirrors the
/// inner branches of Go `applyPromptProtectionRuleToMessage` for the
/// `msg.Content.Content` single-text case. Returns `(new_text, matched)`.
///
/// On `reject` the text is left unchanged (Go short-circuits at the
/// `ApplyPromptProtectionRules` level) and `matched=true` is reported so the
/// caller can record the hit.
fn apply_rule_to_text(
    text: &str,
    pattern: &str,
    settings: &PromptProtectionSettings,
) -> (String, bool) {
    if text.is_empty() || !match_protection_pattern(pattern, text) {
        return (text.to_string(), false);
    }

    let new_text = if settings.action == PROMPT_PROTECTION_ACTION_MASK {
        replace_protection_pattern(pattern, text, settings.replacement.as_deref().unwrap_or(""))
    } else {
        // `reject` — the actual rejection marker is emitted by the caller; the
        // per-message text is unchanged at this layer (Go does the same: the
        // text is only mutated on `mask`).
        text.to_string()
    };

    (new_text, true)
}

/// Decide whether `scopes` applies to `role`. Mirrors Go
/// `promptProtectionRuleAppliesToRole`: empty scopes => applies to every role;
/// otherwise the lowercased role must be one of the scopes.
fn rule_applies_to_role(scopes: &[PromptProtectionScope], role: &str) -> bool {
    if scopes.is_empty() {
        return true;
    }
    let role_scope = role.to_lowercase();
    scopes.iter().any(|s| s == &role_scope)
}

/// Single-pattern preview mirroring Go `PromptProtectionRuleService.Preview`
/// (lines 21-52 of `prompt_protection_preview.go`).
///
/// Errors mirror Go exactly: invalid regex / invalid settings short-circuit
/// before matching. On success returns the preview text and match flag.
pub fn preview_pattern(
    input: &PromptProtectionPreviewInput,
) -> Result<PromptProtectionPreviewResult, String> {
    validate_protection_settings(&input.pattern, &input.settings)?;

    let has_match = match_protection_pattern(&input.pattern, &input.test_text);
    let result = if !has_match {
        input.test_text.clone()
    } else if input.settings.action == PROMPT_PROTECTION_ACTION_MASK {
        replace_protection_pattern(
            &input.pattern,
            &input.test_text,
            input.settings.replacement.as_deref().unwrap_or(""),
        )
    } else {
        // Go: `result = string(objects.PromptProtectionActionReject)` => "reject".
        PROMPT_PROTECTION_ACTION_REJECT.to_string()
    };

    Ok(PromptProtectionPreviewResult { result, has_match })
}

/// Multi-rule preview across a message list. Mirrors Go
/// `ApplyPromptProtectionRules(req, rules)` from
/// `prompt_protection_request.go` lines 24-72, but **additionally** records
/// per-rule / per-message match reasons for the frontend (S12).
///
/// Behavior parity (quoted from Go):
///   * "if `req == nil || len(req.Messages) == 0 || len(rules) == 0` =>
///     `PromptProtectionResult{Request: req}`"
///   * Rules with `nil` settings are skipped (in our encoding, rules whose
///     extra lacks settings — represented here by a rule whose action is empty
///     AND replacement is none AND scopes empty are still processed, mirroring
///     Go's zero-value `&objects.PromptProtectionSettings{}`).
///   * On the first `reject` hit the function returns immediately with
///     `rejected: true`, `messages: None`, and only the rejecting rule in the
///     hits list — mirroring Go's early-return.
///   * Within a rule, every message is evaluated; the rule is recorded as
///     matched when at least one message hits.
///
/// The function takes `&[PromptRule]` and an explicit `settings` lookup
/// closure so callers can attach the Go `Settings` (which our simplified
/// `PromptRule` doesn't yet model) per rule. This keeps the function pure and
/// decoupled from the persistence layer.
pub fn preview_protection(
    messages: &[ChatMessage],
    rules_with_settings: &[(&PromptRule, PromptProtectionSettings)],
) -> ProtectionPreview {
    if messages.is_empty() || rules_with_settings.is_empty() {
        return ProtectionPreview {
            messages: Some(messages.to_vec()),
            rejected: false,
            rules: Vec::new(),
        };
    }

    // Working copy — Go mutates `messages := req.Messages` in place. We start
    // from a clone so the caller's slice is untouched (pure function).
    let mut working: Vec<ChatMessage> = messages.to_vec();
    let mut rule_reports: Vec<ProtectionRuleMatch> = Vec::with_capacity(rules_with_settings.len());

    for (rule, settings) in rules_with_settings {
        let mut hits: Vec<ProtectionRuleHit> = Vec::new();
        let mut rule_matched = false;

        for (i, msg) in working.iter_mut().enumerate() {
            if !rule_applies_to_role(&settings.scopes, &msg.role) {
                continue;
            }

            // --- single-text branch (Go: msg.Content.Content) --------------
            if let Some(MessageContent::Text(text)) = &msg.content
                && !text.is_empty()
            {
                let (new_text, did_match) = apply_rule_to_text(text, &rule.pattern, settings);
                if did_match {
                    rule_matched = true;
                    let after = if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                        PROMPT_PROTECTION_ACTION_REJECT.to_string()
                    } else {
                        new_text.clone()
                    };
                    hits.push(ProtectionRuleHit {
                        message_index: i,
                        role: msg.role.clone(),
                        location: "content".to_string(),
                        before: text.clone(),
                        after,
                    });
                    if settings.action == PROMPT_PROTECTION_ACTION_MASK {
                        msg.content = Some(MessageContent::Text(new_text));
                    }
                    if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                        // Go: return immediately on first reject hit. We also
                        // include earlier rule reports so the UI can still
                        // explain them (S12).
                        rule_reports.push(ProtectionRuleMatch {
                            rule_id: rule.id.clone(),
                            rule_name: rule.name.clone(),
                            pattern: rule.pattern.clone(),
                            action: settings.action.clone(),
                            hits,
                            matched: true,
                        });
                        return ProtectionPreview {
                            messages: None,
                            rejected: true,
                            rules: rule_reports,
                        };
                    }
                }
            }

            // --- multi-part branch (Go: msg.Content.MultipleContent) -------
            if let Some(MessageContent::Parts(parts)) = &mut msg.content {
                for (part_i, part) in parts.iter_mut().enumerate() {
                    if !part.part_type.eq_ignore_ascii_case("text") {
                        continue;
                    }
                    let Some(text) = part.text.as_ref() else {
                        continue;
                    };
                    if text.is_empty() {
                        continue;
                    }
                    let (new_text, did_match) = apply_rule_to_text(text, &rule.pattern, settings);
                    if did_match {
                        rule_matched = true;
                        let after = if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                            PROMPT_PROTECTION_ACTION_REJECT.to_string()
                        } else {
                            new_text.clone()
                        };
                        hits.push(ProtectionRuleHit {
                            message_index: i,
                            role: msg.role.clone(),
                            location: format!("parts[{}]", part_i),
                            before: text.clone(),
                            after,
                        });
                        if settings.action == PROMPT_PROTECTION_ACTION_MASK {
                            part.text = Some(new_text);
                        }
                        if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                            let reject_rule = ProtectionRuleMatch {
                                rule_id: rule.id.clone(),
                                rule_name: rule.name.clone(),
                                pattern: rule.pattern.clone(),
                                action: settings.action.clone(),
                                hits,
                                matched: true,
                            };
                            rule_reports.push(reject_rule);
                            return ProtectionPreview {
                                messages: None,
                                rejected: true,
                                rules: rule_reports,
                            };
                        }
                    }
                }
            }
        }

        rule_reports.push(ProtectionRuleMatch {
            rule_id: rule.id.clone(),
            rule_name: rule.name.clone(),
            pattern: rule.pattern.clone(),
            action: settings.action.clone(),
            hits,
            matched: rule_matched,
        });
    }

    ProtectionPreview {
        messages: Some(working),
        rejected: false,
        rules: rule_reports,
    }
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn invalid_pattern_is_rejected() {
        let repo = Arc::new(InMemoryPromptProtectionPersistenceRepo::new());
        let service = PromptProtectionService::new(repo);
        let ctx = ctx();

        let err = service
            .upsert_rule(
                &ctx,
                PromptRule::new(
                    "rule-1",
                    "invalid",
                    "project-a",
                    PromptRuleStatus::Enabled,
                    10,
                    "[",
                    PromptRuleAction::Block,
                ),
            )
            .await;

        assert!(matches!(
            err,
            Err(PromptProtectionServiceError::InvalidPattern { rule_id, .. })
                if rule_id == "rule-1"
        ));
    }

    #[tokio::test]
    async fn matching_block_rule_blocks_prompt() -> PromptProtectionServiceResult<()> {
        let repo = Arc::new(InMemoryPromptProtectionPersistenceRepo::new());
        let service = PromptProtectionService::new(repo);
        let ctx = ctx();

        service
            .upsert_rule(
                &ctx,
                PromptRule::new(
                    "rule-1",
                    "secret",
                    "project-a",
                    PromptRuleStatus::Enabled,
                    10,
                    "(?i)secret",
                    PromptRuleAction::Block,
                ),
            )
            .await?;

        let decision = service
            .check_prompt(
                &ctx,
                PromptProtectionCheck::new("project-a", "The Secret value is present"),
            )
            .await?;

        assert_eq!(decision.action, PromptRuleAction::Block);
        assert_eq!(decision.matched_rule_id.as_deref(), Some("rule-1"));
        Ok(())
    }

    #[tokio::test]
    async fn lower_order_allow_rule_can_allow_before_block() -> PromptProtectionServiceResult<()> {
        let repo = Arc::new(InMemoryPromptProtectionPersistenceRepo::new());
        let service = PromptProtectionService::new(repo);
        let ctx = ctx();

        service
            .upsert_rule(
                &ctx,
                PromptRule::new(
                    "rule-allow",
                    "allow",
                    "project-a",
                    PromptRuleStatus::Enabled,
                    5,
                    "secret",
                    PromptRuleAction::Allow,
                ),
            )
            .await?;
        service
            .upsert_rule(
                &ctx,
                PromptRule::new(
                    "rule-block",
                    "block",
                    "project-a",
                    PromptRuleStatus::Enabled,
                    10,
                    "secret",
                    PromptRuleAction::Block,
                ),
            )
            .await?;

        let decision = service
            .check_prompt(
                &ctx,
                PromptProtectionCheck::new("project-a", "contains secret"),
            )
            .await?;

        assert_eq!(decision.action, PromptRuleAction::Allow);
        assert_eq!(decision.matched_rule_id.as_deref(), Some("rule-allow"));
        Ok(())
    }

    // =======================================================================
    // Protection preview tests — mirror:
    //   * `TestPromptProtectionRuleService_ValidateSettings`
    //   * `TestPromptProtectionRule_MatchAndReplace`
    //   * `TestPromptProtectionRuleService_Preview`
    //   * `TestApplyPromptProtectionRules*` (MaskContent / RejectContent /
    //     ScopeFiltering / MaskMultipleContent / AppliesMultipleRulesInOrder /
    //     RejectAfterEarlierMask / NoMatchReturnsOriginalRequest)
    // from `prompt_protection_rule_test.go` / `prompt_protection_request_test.go`.
    // =======================================================================

    use conduit_core::objects::prompt_protection::{
        PROMPT_PROTECTION_ACTION_MASK, PROMPT_PROTECTION_ACTION_REJECT,
        PROMPT_PROTECTION_SCOPE_ASSISTANT, PROMPT_PROTECTION_SCOPE_USER,
    };
    use conduit_llm::{ChatMessage, ContentPart, MessageContent};

    fn settings_mask(replacement: &str, scopes: &[&str]) -> PromptProtectionSettings {
        PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_MASK.to_string(),
            replacement: Some(replacement.to_string()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn settings_reject(scopes: &[&str]) -> PromptProtectionSettings {
        PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_REJECT.to_string(),
            replacement: None,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn text_msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    fn parts_msg(role: &str, parts: Vec<ContentPart>) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Parts(parts)),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    fn text_part(text: &str) -> ContentPart {
        ContentPart {
            part_type: "text".to_string(),
            text: Some(text.to_string()),
            image_url: None,
            input_audio: None,
            extra: BTreeMap::new(),
        }
    }

    fn text_of(msg: &ChatMessage) -> Option<&str> {
        match msg.content.as_ref()? {
            MessageContent::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn rule(id: &str, name: &str, pattern: &str) -> PromptRule {
        PromptRule::new(
            id,
            name,
            "proj",
            PromptRuleStatus::Enabled,
            0,
            pattern,
            PromptRuleAction::Block,
        )
    }

    fn unwrap_messages(preview: &ProtectionPreview) -> &[ChatMessage] {
        match &preview.messages {
            Some(msgs) => msgs.as_slice(),
            None => &[],
        }
    }

    // --- ValidateSettings parity ------------------------------------------

    #[test]
    fn validate_settings_invalid_regex_errors() {
        let err = validate_protection_settings("[", &settings_reject(&["system"]));
        match err {
            Err(msg) => assert!(msg.contains("invalid regex pattern"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_settings_invalid_action_errors() {
        let bad = PromptProtectionSettings {
            action: "unknown".to_string(),
            replacement: None,
            scopes: Vec::new(),
        };
        match validate_protection_settings("secret", &bad) {
            Err(msg) => assert!(msg.contains("invalid action"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_settings_mask_requires_replacement() {
        let bad = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_MASK.to_string(),
            replacement: None,
            scopes: Vec::new(),
        };
        match validate_protection_settings("secret", &bad) {
            Err(msg) => assert!(msg.contains("replacement is required"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_settings_invalid_scope_errors() {
        let bad = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_REJECT.to_string(),
            replacement: None,
            scopes: vec!["bad".to_string()],
        };
        match validate_protection_settings("secret", &bad) {
            Err(msg) => assert!(msg.contains("invalid scope"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_settings_valid_mask_and_reject_pass() {
        assert!(validate_protection_settings("secret", &settings_mask("***", &["user"])).is_ok());
        assert!(validate_protection_settings("secret", &settings_reject(&["system"])).is_ok());
    }

    // --- MatchAndReplace parity -------------------------------------------

    #[test]
    fn match_protection_pattern_invalid_pattern_is_false() {
        assert!(!match_protection_pattern("[", "anything"));
    }

    #[test]
    fn replace_protection_pattern_invalid_pattern_returns_original() {
        assert_eq!(replace_protection_pattern("[", "anything", "x"), "anything");
    }

    #[test]
    fn match_protection_pattern_substring_match() {
        // Go's MatchPromptProtectionRule uses regexp2.Find (unanchored), so a
        // substring match is a hit. Our regex::Regex::is_match also does
        // unanchored matching — parity holds.
        assert!(match_protection_pattern("secret", "my secret is here"));
        assert!(!match_protection_pattern("secret", "nothing to see"));
    }

    #[test]
    fn replace_protection_pattern_replaces_all_occurrences() {
        assert_eq!(
            replace_protection_pattern("secret", "my secret is here", "***"),
            "my *** is here"
        );
    }

    // --- Preview (single-pattern) parity ----------------------------------

    #[test]
    fn preview_mask_action_returns_replaced_preview() {
        let result = match preview_pattern(&PromptProtectionPreviewInput {
            pattern: r"secret-[0-9]+".to_string(),
            test_text: "token is secret-123".to_string(),
            settings: settings_mask("[MASKED]", &[PROMPT_PROTECTION_SCOPE_USER]),
        }) {
            Ok(r) => r,
            Err(e) => panic!("preview failed: {e}"),
        };
        assert!(result.has_match);
        assert_eq!(result.result, "token is [MASKED]");
    }

    #[test]
    fn preview_reject_action_returns_reject_marker() {
        let result = match preview_pattern(&PromptProtectionPreviewInput {
            pattern: "secret".to_string(),
            test_text: "contains secret".to_string(),
            settings: settings_reject(&[PROMPT_PROTECTION_SCOPE_USER]),
        }) {
            Ok(r) => r,
            Err(e) => panic!("preview failed: {e}"),
        };
        assert!(result.has_match);
        assert_eq!(result.result, PROMPT_PROTECTION_ACTION_REJECT);
    }

    #[test]
    fn preview_invalid_pattern_returns_error() {
        let err = preview_pattern(&PromptProtectionPreviewInput {
            pattern: "(".to_string(),
            test_text: "anything".to_string(),
            settings: settings_reject(&[PROMPT_PROTECTION_SCOPE_USER]),
        });
        assert!(err.is_err());
    }

    #[test]
    fn preview_no_match_returns_original_text() {
        let result = match preview_pattern(&PromptProtectionPreviewInput {
            pattern: "secret".to_string(),
            test_text: "nothing here".to_string(),
            settings: settings_mask("[MASKED]", &[PROMPT_PROTECTION_SCOPE_USER]),
        }) {
            Ok(r) => r,
            Err(e) => panic!("preview failed: {e}"),
        };
        assert!(!result.has_match);
        assert_eq!(result.result, "nothing here");
    }

    // --- ApplyPromptProtectionRules parity (multi-rule preview) -----------

    #[test]
    fn preview_protection_empty_messages_returns_empty() {
        let result = preview_protection(&[], &[]);
        assert!(!result.rejected);
        assert!(result.messages.is_some());
        assert!(unwrap_messages(&result).is_empty());
        assert!(result.rules.is_empty());
    }

    #[test]
    fn preview_protection_masks_single_text_message() {
        // Mirrors Go TestApplyPromptProtectionRulesMaskContent.
        let messages = vec![text_msg("user", "token is secret-123")];
        let mask_rule = rule("mask-secret", "mask-secret", r"secret-[0-9]+");
        let result = preview_protection(
            &messages,
            &[(
                &mask_rule,
                settings_mask("[MASKED]", &[PROMPT_PROTECTION_SCOPE_USER]),
            )],
        );

        assert!(!result.rejected);
        assert_eq!(result.rules.len(), 1);
        assert!(result.rules[0].matched);
        let final_msgs = unwrap_messages(&result);
        assert_eq!(text_of(&final_msgs[0]), Some("token is [MASKED]"));
    }

    #[test]
    fn preview_protection_reject_marks_rejected_and_drops_messages() {
        // Mirrors Go TestApplyPromptProtectionRulesRejectContent.
        let messages = vec![text_msg("user", "contains secret")];
        let reject_rule = rule("reject-secret", "reject-secret", "secret");
        let result = preview_protection(
            &messages,
            &[(
                &reject_rule,
                settings_reject(&[PROMPT_PROTECTION_SCOPE_USER]),
            )],
        );

        assert!(result.rejected);
        // Go: assert.Nil(t, result.Request) — we mirror with messages=None.
        assert!(result.messages.is_none());
        assert_eq!(result.rules.len(), 1);
        assert!(result.rules[0].matched);
        assert_eq!(result.rules[0].rule_name, "reject-secret");
    }

    #[test]
    fn preview_protection_scope_filtering_skips_non_matching_role() {
        // Mirrors Go TestApplyPromptProtectionRulesScopeFiltering: a user-only
        // rule must NOT fire on an assistant message.
        let messages = vec![text_msg("assistant", "contains secret")];
        let user_only = rule("user-only", "user-only", "secret");
        let result = preview_protection(
            &messages,
            &[(
                &user_only,
                settings_mask("[MASKED]", &[PROMPT_PROTECTION_SCOPE_USER]),
            )],
        );

        assert!(!result.rejected);
        assert!(result.rules[0].hits.is_empty());
        assert!(!result.rules[0].matched);
        let final_msgs = unwrap_messages(&result);
        assert_eq!(text_of(&final_msgs[0]), Some("contains secret"));
    }

    #[test]
    fn preview_protection_masks_text_parts_in_multi_part_message() {
        // Mirrors Go TestApplyPromptProtectionRulesMaskMultipleContent.
        let messages = vec![parts_msg("user", vec![text_part("secret text")])];
        let mask_part = rule("mask-part", "mask-part", "secret");
        let result = preview_protection(&messages, &[(&mask_part, settings_mask("[MASKED]", &[]))]);

        assert!(!result.rejected);
        assert_eq!(result.rules.len(), 1);
        assert!(result.rules[0].matched);
        assert_eq!(result.rules[0].hits[0].location, "parts[0]");
        let final_msgs = unwrap_messages(&result);
        match &final_msgs[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts[0].text.as_deref(), Some("[MASKED] text"));
            }
            _ => panic!("expected Parts content"),
        }
    }

    #[test]
    fn preview_protection_applies_multiple_rules_in_order() {
        // Mirrors Go TestApplyPromptProtectionRules_AppliesMultipleRulesInOrder.
        let messages = vec![text_msg("user", "alice secret")];
        let mask_name = rule("mask-name", "mask-name", "alice");
        let mask_secret = rule("mask-secret", "mask-secret", "secret");
        let result = preview_protection(
            &messages,
            &[
                (&mask_name, settings_mask("[USER]", &[])),
                (&mask_secret, settings_mask("[MASKED]", &[])),
            ],
        );

        assert!(!result.rejected);
        assert_eq!(result.rules.len(), 2);
        assert!(result.rules[0].matched);
        assert!(result.rules[1].matched);
        assert_eq!(result.rules[0].rule_name, "mask-name");
        assert_eq!(result.rules[1].rule_name, "mask-secret");
        let final_msgs = unwrap_messages(&result);
        assert_eq!(text_of(&final_msgs[0]), Some("[USER] [MASKED]"));
    }

    #[test]
    fn preview_protection_reject_after_earlier_mask_short_circuits() {
        // Mirrors Go TestApplyPromptProtectionRules_RejectAfterEarlierMask:
        // first rule masks "secret" -> "[MASKED]", second rule rejects on the
        // masked token. The result must be rejected; both rule reports are
        // included (S12 surface), but only the reject rule is flagged matched.
        let messages = vec![text_msg("user", "secret")];
        let mask_secret = rule("mask-secret", "mask-secret", "secret");
        let reject_masked = rule("reject-masked", "reject-masked", r"\[MASKED\]");
        let result = preview_protection(
            &messages,
            &[
                (&mask_secret, settings_mask("[MASKED]", &[])),
                (&reject_masked, settings_reject(&[])),
            ],
        );

        assert!(result.rejected);
        assert!(result.messages.is_none());

        let reject_report = match result.rules.iter().find(|r| r.rule_id == "reject-masked") {
            Some(r) => r,
            None => panic!("reject rule report present"),
        };
        assert!(reject_report.matched);
        assert_eq!(reject_report.action, PROMPT_PROTECTION_ACTION_REJECT);
    }

    #[test]
    fn preview_protection_no_match_returns_original_messages() {
        // Mirrors Go TestApplyPromptProtectionRules_NoMatchReturnsOriginalRequest.
        let messages = vec![text_msg("user", "hello world")];
        let mask_secret = rule("mask-secret", "mask-secret", "secret");
        let result =
            preview_protection(&messages, &[(&mask_secret, settings_mask("[MASKED]", &[]))]);

        assert!(!result.rejected);
        // Go: assert.Nil(t, result.MatchedRules). We include the rule report
        // (S12) but it must be `matched: false` with no hits.
        assert!(!result.rules[0].matched);
        assert!(result.rules[0].hits.is_empty());
        let final_msgs = unwrap_messages(&result);
        assert_eq!(text_of(&final_msgs[0]), Some("hello world"));
    }

    #[test]
    fn preview_protection_empty_scopes_apply_to_all_roles() {
        // Go promptProtectionRuleAppliesToRole: empty scopes => applies to every role.
        let messages = vec![text_msg(PROMPT_PROTECTION_SCOPE_ASSISTANT, "top secret")];
        let any_role = rule("any", "any", "secret");
        let result = preview_protection(&messages, &[(&any_role, settings_mask("[X]", &[]))]);

        assert!(result.rules[0].matched);
    }

    // =======================================================================
    // S10 — CompiledRules (regex compile-cache) tests
    //
    // Mirror Go `TestPromptProtectionRuleService_CreateRule` /
    // `TestPromptProtectionRuleService_UpdateRule` parity: an invalid pattern
    // must be rejected at build time (Go rejects at Create/Update via
    // `ValidateSettings` -> `getOrCompilePromptProtectionPattern` returning
    // the cached compile error).
    // =======================================================================

    fn build_inputs<'a>(
        rules: &'a [PromptRule],
        settings: &'a PromptProtectionSettings,
    ) -> Vec<CompiledRuleInput<'a>> {
        rules
            .iter()
            .map(|r| CompiledRuleInput {
                rule_id: r.id.clone(),
                pattern: r.pattern.clone(),
                settings: settings.clone(),
                rule: r,
            })
            .collect()
    }

    #[test]
    fn compiled_rules_build_ok_for_valid_patterns() {
        // Go parity: valid patterns compile and the cache is built.
        let rules = vec![
            rule("r1", "mask-secret", r"secret-[0-9]+"),
            rule("r2", "mask-name", "alice"),
        ];
        let settings = settings_mask("[MASKED]", &[]);
        let compiled = match CompiledRules::build(&build_inputs(&rules, &settings)) {
            Ok(c) => c,
            Err(e) => panic!("build failed: {e}"),
        };
        assert_eq!(compiled.entries().len(), 2);
        assert_eq!(compiled.entries()[0].rule_id, "r1");
        assert_eq!(compiled.entries()[1].rule_id, "r2");
        // find() parity: cache lookup by rule id.
        assert!(compiled.find("r1").is_some());
        assert!(compiled.find("missing").is_none());
    }

    #[test]
    fn compiled_rules_build_rejects_invalid_pattern_with_rule_id() {
        // Go parity: `TestPromptProtectionRuleService_CreateRule` /
        // `UpdateRule` reject `"["` with `"invalid regex pattern"`.
        let rules = vec![rule("bad-rule", "bad", "[")];
        let settings = settings_reject(&[]);
        let err = CompiledRules::build(&build_inputs(&rules, &settings))
            .err()
            .unwrap_or_else(|| panic!("expected PatternCompileError"));
        assert_eq!(err.rule_id, "bad-rule");
        assert_eq!(err.pattern, "[");
        assert!(
            err.message.contains("regex"),
            "message should mention regex: {err}"
        );
    }

    #[test]
    fn compiled_rules_build_rejects_first_invalid_pattern() {
        // Go parity: ValidateSettings short-circuits on the FIRST bad pattern.
        let rules = vec![rule("good", "good", "secret"), rule("bad", "bad", "(")];
        let settings = settings_mask("[X]", &[]);
        let err = CompiledRules::build(&build_inputs(&rules, &settings))
            .err()
            .unwrap_or_else(|| panic!("expected PatternCompileError"));
        // Must report the bad rule, not the good one.
        assert_eq!(err.rule_id, "bad");
    }

    // =======================================================================
    // S06 — validate_protection_rule (create/update-time validation) tests
    //
    // Mirror Go `TestPromptProtectionRuleService_ValidateSettings` table
    // (invalid_regex / nil_settings / invalid_action / mask_requires_replacement
    // / invalid_scope / valid_mask / valid_reject) plus the implicit
    // pattern-compile gate that Create/Update exercise.
    // =======================================================================

    fn active_rule(id: &str, pattern: &str) -> PromptRule {
        PromptRule::new(
            id,
            id,
            "proj",
            PromptRuleStatus::Enabled,
            0,
            pattern,
            PromptRuleAction::Block,
        )
    }

    #[test]
    fn validate_protection_rule_invalid_pattern_errors() {
        // Go parity: wantErr "invalid regex pattern".
        let rule = active_rule("r1", "[");
        let settings = settings_reject(&["system"]);
        match validate_protection_rule(&rule, &settings) {
            Err(msg) => assert!(msg.contains("invalid regex pattern"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_protection_rule_invalid_action_errors() {
        // Go parity: wantErr "invalid action".
        let rule = active_rule("r1", "secret");
        let bad = PromptProtectionSettings {
            action: "unknown".to_string(),
            replacement: None,
            scopes: Vec::new(),
        };
        match validate_protection_rule(&rule, &bad) {
            Err(msg) => assert!(msg.contains("invalid action"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_protection_rule_mask_requires_replacement() {
        // Go parity: wantErr "replacement is required".
        let rule = active_rule("r1", "secret");
        let bad = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_MASK.to_string(),
            replacement: None,
            scopes: Vec::new(),
        };
        match validate_protection_rule(&rule, &bad) {
            Err(msg) => assert!(msg.contains("replacement is required"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_protection_rule_invalid_scope_errors() {
        // Go parity: wantErr "invalid scope".
        let rule = active_rule("r1", "secret");
        let bad = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_REJECT.to_string(),
            replacement: None,
            scopes: vec!["bad".to_string()],
        };
        match validate_protection_rule(&rule, &bad) {
            Err(msg) => assert!(msg.contains("invalid scope"), "got: {msg}"),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn validate_protection_rule_valid_mask_and_reject_pass() {
        // Go parity: valid_mask / valid_reject rows return NoError.
        let rule = active_rule("r1", "secret");
        assert!(validate_protection_rule(&rule, &settings_mask("***", &["user"])).is_ok());
        assert!(validate_protection_rule(&rule, &settings_reject(&["system"])).is_ok());
    }

    // =======================================================================
    // S11 — decide_protection (block-on-match decision) tests
    //
    // Mirror Go `ApplyPromptProtectionRules` semantics at the single-text
    // level: rules evaluated in order, first reject short-circuits with
    // Block, masks accumulate, no match => Allow.
    // =======================================================================

    fn compiled(rules: &[PromptRule], settings: &PromptProtectionSettings) -> CompiledRules {
        match CompiledRules::build(&build_inputs(rules, settings)) {
            Ok(c) => c,
            Err(e) => panic!("build failed: {e}"),
        }
    }

    #[test]
    fn decide_protection_no_rules_is_allow() {
        // Go parity: len(rules)==0 => pass through unchanged.
        let empty = CompiledRules::default();
        let decision = decide_protection("anything", &empty);
        assert!(decision.is_allow());
        assert_eq!(decision, ProtectionDecision::Allow);
    }

    #[test]
    fn decide_protection_no_match_is_allow() {
        // Go parity: TestApplyPromptProtectionRules_NoMatchReturnsOriginalRequest.
        let rules = vec![rule("r1", "secret", "secret")];
        let settings = settings_mask("[X]", &[]);
        let decision = decide_protection("hello world", &compiled(&rules, &settings));
        assert!(decision.is_allow());
    }

    #[test]
    fn decide_protection_mask_rule_returns_masked_text() {
        // Go parity: TestApplyPromptProtectionRulesMaskContent — masked text
        // is returned for forwarding to the provider.
        let rules = vec![rule("r1", "mask-secret", r"secret-[0-9]+")];
        let settings = settings_mask("[MASKED]", &[]);
        let decision = decide_protection("token is secret-123", &compiled(&rules, &settings));
        match decision {
            ProtectionDecision::Mask(new_text) => assert_eq!(new_text, "token is [MASKED]"),
            other => panic!("expected Mask, got {other:?}"),
        }
    }

    #[test]
    fn decide_protection_reject_rule_blocks_with_rule_id() {
        // Go parity: TestApplyPromptProtectionRulesRejectContent — first
        // reject short-circuits, request is NOT forwarded (Block).
        let rules = vec![rule("reject-secret", "reject-secret", "secret")];
        let settings = settings_reject(&[]);
        let decision = decide_protection("contains secret", &compiled(&rules, &settings));
        assert!(decision.is_block());
        match decision {
            ProtectionDecision::Block { rule_id, reason } => {
                assert_eq!(rule_id, "reject-secret");
                // Go ErrPromptProtectionRejected = "prompt protection rejected request".
                assert!(reason.contains("rejected"), "got: {reason}");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn decide_protection_masks_accumulate_across_multiple_rules() {
        // Go parity: TestApplyPromptProtectionRules_AppliesMultipleRulesInOrder
        // — earlier mask feeds into later rule's input text.
        let rules = vec![
            rule("mask-name", "mask-name", "alice"),
            rule("mask-secret", "mask-secret", "secret"),
        ];
        let settings = settings_mask("[X]", &[]);
        let decision = decide_protection("alice secret", &compiled(&rules, &settings));
        match decision {
            ProtectionDecision::Mask(new_text) => assert_eq!(new_text, "[X] [X]"),
            other => panic!("expected Mask, got {other:?}"),
        }
    }

    #[test]
    fn decide_protection_reject_after_mask_short_circuits() {
        // Go parity: TestApplyPromptProtectionRules_RejectAfterEarlierMask —
        // first rule masks, second rule rejects on the masked token. Result
        // is Block (reject wins over mask).
        let rules = [
            rule("mask-secret", "mask-secret", "secret"),
            rule("reject-masked", "reject-masked", r"\[X\]"),
        ];
        // First rule masks, second rule rejects — we need per-rule settings,
        // so build the cache by hand with two different settings.
        let mask_settings = settings_mask("[X]", &[]);
        let reject_settings = settings_reject(&[]);
        let inputs = vec![
            CompiledRuleInput {
                rule_id: "mask-secret".to_string(),
                pattern: "secret".to_string(),
                settings: mask_settings,
                rule: &rules[0],
            },
            CompiledRuleInput {
                rule_id: "reject-masked".to_string(),
                pattern: r"\[X\]".to_string(),
                settings: reject_settings,
                rule: &rules[1],
            },
        ];
        let cache = match CompiledRules::build(&inputs) {
            Ok(c) => c,
            Err(e) => panic!("build failed: {e}"),
        };
        let decision = decide_protection("secret", &cache);
        match decision {
            ProtectionDecision::Block { rule_id, .. } => assert_eq!(rule_id, "reject-masked"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn decide_protection_empty_text_is_allow() {
        // Edge: empty text never matches — parity with Go's empty-message
        // skip in applyPromptProtectionRuleToMessage (Content != "" gate).
        let rules = vec![rule("r1", "reject", "secret")];
        let settings = settings_reject(&[]);
        let decision = decide_protection("", &compiled(&rules, &settings));
        assert!(decision.is_allow());
    }

    // =======================================================================
    // S13 — condition-evaluator whitelist (no user-script execution).
    //
    // Mirrors Go's invariant that `prompt_protection_rule.go` ONLY ever
    // compiles a regex pattern (`regexp2.Compile`) and runs `MatchString` /
    // `Replace`. It NEVER calls `objects.Evaluate` / `expr.Compile` / any
    // script engine, even though the wider Go codebase ships `expr-lang/expr`
    // via `internal/objects/condition.go` (used by model filters, candidate
    // selection, prompt activation, and GraphQL filters). The whitelist gate
    // [`reject_forbidden_evaluators`] enforces that invariant at rule
    // validation time, before any dispatch.
    // =======================================================================

    #[test]
    fn s13_whitelist_admits_only_regex_pattern() {
        // Stock Go prompt protection: the only admitted evaluator is the
        // regex-pattern path. Everything else (Condition / Expression /
        // Script) MUST be rejected.
        assert!(is_allowed_protection_evaluator(
            ProtectionRuleEvaluator::RegexPattern
        ));
        assert!(!is_allowed_protection_evaluator(
            ProtectionRuleEvaluator::Condition
        ));
        assert!(!is_allowed_protection_evaluator(
            ProtectionRuleEvaluator::Expression
        ));
        assert!(!is_allowed_protection_evaluator(
            ProtectionRuleEvaluator::Script
        ));
        // Sanity: the whitelist constant is the single-element slice we claim.
        assert_eq!(
            PROTECTION_ALLOWED_EVALUATORS,
            &[ProtectionRuleEvaluator::RegexPattern]
        );
    }

    #[test]
    fn s13_pure_regex_rule_is_admitted() {
        // A rule carrying only the stock fields (pattern + action + status +
        // the rule's own metadata) is admitted. No `condition` / `expression`
        // / `script` field in `extra`.
        let rule = active_rule("r1", "secret");
        let classification = classify_protection_rule_kind(&rule);
        assert_eq!(
            classification,
            ProtectionRuleClassification::Admitted(ProtectionRuleEvaluator::RegexPattern)
        );
        // The security gate lets it through.
        assert!(reject_forbidden_evaluators(&rule).is_ok());
        // And validate_protection_rule (which now wires the gate) returns Ok
        // for a fully-valid rule.
        let settings = settings_reject(&["system"]);
        assert!(validate_protection_rule(&rule, &settings).is_ok());
    }

    #[test]
    fn s13_rule_carrying_condition_field_is_rejected() {
        // A rule that smuggles a `condition` field into `extra` (the shape
        // `objects.Condition` would consume) MUST be rejected before any
        // dispatch. The error must name the offending field and the evaluator
        // it implies.
        let mut rule = active_rule("evil-1", "secret");
        rule.extra
            .insert("condition".to_string(), serde_json::json!({"field": "x"}));

        match classify_protection_rule_kind(&rule) {
            ProtectionRuleClassification::Forbidden { field, implies } => {
                assert_eq!(field, "condition");
                assert_eq!(implies, ProtectionRuleEvaluator::Condition);
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }

        let err = match reject_forbidden_evaluators(&rule) {
            Err(err) => err,
            Ok(_) => panic!("expected ForbiddenEvaluatorError"),
        };
        assert_eq!(err.rule_id, "evil-1");
        assert_eq!(err.field, "condition");
        assert_eq!(err.implies, ProtectionRuleEvaluator::Condition);

        // validate_protection_rule surfaces the gate as a String error.
        let settings = settings_reject(&["system"]);
        match validate_protection_rule(&rule, &settings) {
            Err(msg) => {
                assert!(msg.contains("forbidden evaluator"), "got: {msg}");
                assert!(msg.contains("condition"), "got: {msg}");
                assert!(msg.contains("evil-1"), "got: {msg}");
            }
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn s13_rule_carrying_expression_or_script_fields_is_rejected() {
        // Each forbidden field (expression / expr / script / code / eval) is
        // independently rejected. The gate is closed-world: any field in
        // FORBIDDEN_EVALUATOR_FIELDS fails the rule.
        for (field, expected_implies) in [
            ("expression", ProtectionRuleEvaluator::Expression),
            ("expr", ProtectionRuleEvaluator::Expression),
            ("script", ProtectionRuleEvaluator::Script),
            ("code", ProtectionRuleEvaluator::Script),
            ("eval", ProtectionRuleEvaluator::Script),
            ("conditions", ProtectionRuleEvaluator::Condition),
        ] {
            let mut rule = active_rule("evil-x", "secret");
            rule.extra
                .insert(field.to_string(), serde_json::json!("payload"));

            match reject_forbidden_evaluators(&rule) {
                Err(err) => {
                    assert_eq!(err.field, field, "field {field} not rejected");
                    assert_eq!(err.implies, expected_implies);
                }
                Ok(_) => panic!("field {field} should have been rejected"),
            }
        }
    }

    #[test]
    fn s13_forbidden_gate_runs_before_pattern_compile() {
        // Security ordering: the S13 gate MUST run BEFORE the regex-compile
        // check, so a rule carrying BOTH a forbidden evaluator AND an invalid
        // pattern reports the forbidden-evaluator error (not the regex error)
        // — never executes the smuggled evaluator. This pins the order
        // `reject_forbidden_evaluators` -> `validate_protection_settings`.
        let mut rule = active_rule("evil-2", "["); // intentionally bad regex
        rule.extra
            .insert("script".to_string(), serde_json::json!("rm -rf /"));

        let settings = settings_reject(&["system"]);
        match validate_protection_rule(&rule, &settings) {
            Err(msg) => {
                // Forbidden-evaluator error wins over the regex-compile error.
                assert!(
                    msg.contains("forbidden evaluator"),
                    "expected forbidden-evaluator error first, got: {msg}"
                );
                assert!(msg.contains("script"), "got: {msg}");
                // The regex-compile error is NOT what we see first.
                assert!(!msg.starts_with("invalid regex pattern"));
            }
            Ok(_) => panic!("expected error"),
        }
    }

    // =======================================================================
    // RUST-P15-001 S03 — additional prompt_protection golden-case ports.
    //
    // These tests fill the remaining pure-logic gaps from
    //   * `prompt_protection_rule_test.go`    (L1-407)
    //   * `prompt_protection_request_test.go` (L1-293)
    // that were not already covered by the S06-S13 blocks above.
    //
    // Go tests that remain pending are the DB-backed ent subtests:
    //   * TestPromptProtectionRuleService_CreateRule                 (L134-178)
    //   * TestPromptProtectionRuleService_CreateRule_SettingsRequired (L180-191)
    //   * TestPromptProtectionRuleService_UpdateRule                 (L193-237)
    //   * TestPromptProtectionRuleService_DeleteRule                 (L239-269)
    //   * TestPromptProtectionRuleService_UpdateRuleStatus           (L271-288)
    //   * TestPromptProtectionRuleService_BulkOpsAndListEnabled      (L290-357)
    //   * TestPromptProtectionRuleService_ProtectMask                (L222-252)
    //   * TestPromptProtectionRuleService_ProtectReject              (L254-281)
    //   * TestPromptProtectionRuleService_ProtectLoadError           (L283-293)
    // Pending DB-backed: the original Go tests use `enttest.NewEntClient`;
    // the Rust equivalents require a PostgreSQL-backed integration fixture.
    // =======================================================================

    // --- Parity bug: regexp2 lookbehind (Go L129-131) --------------------
    //
    // PARITY BUG (report):
    //   Go file: conduit/internal/server/biz/prompt_protection_rule_test.go L130-131
    //   Go source: conduit/internal/server/biz/prompt_protection_rule.go L162
    //   Rust file: crates/conduit-services/src/prompt_protection_service.rs L382
    //
    // Go uses `github.com/dlclark/regexp2/v2` (line 9 of prompt_protection_rule.go)
    // which is a .NET-compatible engine supporting lookbehind `(?<!...)` and
    // lookahead `(?=...)`. The Rust port uses the `regex` crate (Cargo.toml L27:
    // `regex = "1"`), which explicitly does NOT support lookaround. The Go
    // golden case at L130-131 compiles a lookbehind email pattern and asserts
    // it matches / replaces. Under the Rust `regex` crate the pattern fails to
    // compile, so `match_protection_pattern` returns `false` (Go: `true`) and
    // `replace_protection_pattern` returns the original content (Go:
    // `"email: [EMAIL]"`).
    //
    // Fix would require switching to `fancy-regex` (which supports lookaround)
    // — flagged for Leader; do NOT change production impl in this slice.
    //
    // The two tests below pin the CURRENT Rust behavior so the gap is
    // detectable in the test suite. They are NOT marked `#[ignore]` — they
    // pass and document the divergence.

    /// Mirrors Go `TestPromptProtectionRule_MatchAndReplace` L130 (lookbehind
    /// email match). Go expects `true`; Rust returns `false` because the
    /// `regex` crate cannot compile lookbehind. Pinned to document the gap.
    #[test]
    fn match_protection_pattern_lookbehind_email_parity_gap() {
        let pattern = r"(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?=$|[^A-Za-z0-9._%+-])";
        let content = "email: test@example.com";
        // Go (regexp2): returns true. Rust (regex crate): returns false
        // because the lookbehind fails to compile.
        assert!(
            !match_protection_pattern(pattern, content),
            "PARITY GAP: Go regexp2 matches this lookbehind pattern but the \
             Rust `regex` crate cannot compile it; consider `fancy-regex`"
        );
    }

    /// Mirrors Go `TestPromptProtectionRule_MatchAndReplace` L131 (lookbehind
    /// email replace). Go expects `"email: [EMAIL]"`; Rust returns the
    /// original content because the `regex` crate cannot compile lookbehind.
    /// Pinned to document the gap.
    #[test]
    fn replace_protection_pattern_lookbehind_email_parity_gap() {
        let pattern = r"(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?=$|[^A-Za-z0-9._%+-])";
        let content = "email: test@example.com";
        // Go (regexp2): returns "email: [EMAIL]". Rust (regex crate): returns
        // the original content because the lookbehind fails to compile.
        assert_eq!(
            replace_protection_pattern(pattern, content, "[EMAIL]"),
            content,
            "PARITY GAP: Go regexp2 replaces with [EMAIL] but the Rust \
             `regex` crate cannot compile the lookbehind"
        );
    }

    // --- Replace semantics: multiple occurrences -------------------------
    //
    // Mirrors the semantics of Go `ReplacePromptProtectionRule` (L139-151)
    // which calls `re.Replace(content, replacement, -1, -1)` — the `-1, -1`
    // means "replace all occurrences, starting from the beginning". The
    // existing `replace_protection_pattern_replaces_all_occurrences` test only
    // has one occurrence in the input; this test verifies multiple occurrences
    // are all replaced.

    /// Verifies `replace_protection_pattern` replaces ALL occurrences, mirroring
    /// Go `re.Replace(content, replacement, -1, -1)` semantics.
    #[test]
    fn replace_protection_pattern_replaces_multiple_occurrences() {
        assert_eq!(
            replace_protection_pattern("secret", "secret one and secret two", "***"),
            "*** one and *** two"
        );
    }

    // --- Multi-part message: part count preservation (Go L123) -----------
    //
    // Mirrors Go `TestApplyPromptProtectionRulesMaskMultipleContent` L123:
    //   require.Len(t, result.Request.Messages[0].Content.MultipleContent, 1)
    // The existing `preview_protection_masks_text_parts_in_multi_part_message`
    // test checks the replaced text but does not explicitly assert the part
    // count is preserved.

    /// Mirrors Go `TestApplyPromptProtectionRulesMaskMultipleContent` L123 —
    /// the multi-part slice length must be preserved after masking.
    #[test]
    fn preview_protection_multi_part_preserves_part_count() {
        let messages = vec![parts_msg("user", vec![text_part("secret text")])];
        let mask_part = rule("mask-part", "mask-part", "secret");
        let result = preview_protection(&messages, &[(&mask_part, settings_mask("[MASKED]", &[]))]);

        let final_msgs = unwrap_messages(&result);
        match &final_msgs[0].content {
            Some(MessageContent::Parts(parts)) => {
                // Go L123: require.Len(..., 1)
                assert_eq!(parts.len(), 1, "part count must be preserved");
                assert_eq!(parts[0].text.as_deref(), Some("[MASKED] text"));
            }
            _ => panic!("expected Parts content"),
        }
    }

    // --- Multi-part message: non-text parts untouched --------------------
    //
    // Mirrors the Go loop guard in `applyPromptProtectionRuleToMessage` L124:
    //   if !strings.EqualFold(part.Type, "text") || part.Text == nil || *part.Text == "" {
    //       continue
    //   }
    // Non-text parts (image_url, input_audio) must be skipped and preserved
    // untouched. No Go golden case explicitly tests this with a mixed list,
    // but it is an implicit invariant of the Go loop.

    /// Verifies that non-text parts in a multi-part message are preserved
    /// untouched when a mask rule fires on a sibling text part. Mirrors the
    /// Go loop guard `!strings.EqualFold(part.Type, "text")` at L124 of
    /// `prompt_protection_request.go`.
    #[test]
    fn preview_protection_multi_part_preserves_non_text_parts() {
        let image_part = ContentPart {
            part_type: "image_url".to_string(),
            text: None,
            image_url: Some(serde_json::json!({"url": "https://example.com/img.png"})),
            input_audio: None,
            extra: BTreeMap::new(),
        };
        let messages = vec![parts_msg(
            "user",
            vec![text_part("secret text"), image_part],
        )];
        let mask_part = rule("mask-part", "mask-part", "secret");
        let result = preview_protection(&messages, &[(&mask_part, settings_mask("[MASKED]", &[]))]);

        assert!(!result.rejected);
        let final_msgs = unwrap_messages(&result);
        match &final_msgs[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2, "both parts must be preserved");
                // Text part is masked.
                assert_eq!(parts[0].text.as_deref(), Some("[MASKED] text"));
                // Image part is untouched.
                assert_eq!(parts[1].part_type, "image_url");
                assert!(parts[1].image_url.is_some(), "image_url must be preserved");
                assert!(parts[1].text.is_none(), "image part text must remain None");
            }
            _ => panic!("expected Parts content"),
        }
    }

    // --- ValidateSettings: empty replacement string (Go L78-83) ----------
    //
    // Mirrors Go `TestPromptProtectionRuleService_ValidateSettings` L78-83
    // (`mask_requires_replacement` case). Go's `Replacement` is a plain
    // `string` (zero value `""`); the Rust port models it as `Option<String>`.
    // The existing `validate_settings_mask_requires_replacement` test checks
    // `None`; this test checks `Some("")` — both map to Go's empty string and
    // must trigger the same error.

    /// Mirrors Go `mask_requires_replacement` — `Some("")` (the wire-level
    /// equivalent of Go's empty-string `Replacement`) must also be rejected.
    #[test]
    fn validate_protection_settings_mask_with_empty_replacement_string() {
        let bad = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_MASK.to_string(),
            replacement: Some(String::new()), // Some("") — maps to Go's ""
            scopes: Vec::new(),
        };
        match validate_protection_settings("secret", &bad) {
            Err(msg) => assert!(msg.contains("replacement is required"), "got: {msg}"),
            Ok(_) => panic!("expected error for empty replacement string"),
        }
    }

    // --- Scope matching: case-insensitive role (Go L143-150) -------------
    //
    // Mirrors Go `promptProtectionRuleAppliesToRole` L143-150:
    //   roleScope := objects.PromptProtectionScope(strings.ToLower(role))
    //   return slices.Contains(scopes, roleScope)
    // The role is lowercased before checking against scopes. None of the
    // existing tests exercise a mixed-case role like "User" or "ASSISTANT".
    // This test verifies the case-insensitive matching.

    /// Verifies that `rule_applies_to_role` lowercases the role before
    /// matching, mirroring Go `strings.ToLower(role)` at L148 of
    /// `prompt_protection_request.go`.
    #[test]
    fn rule_applies_to_role_is_case_insensitive() {
        // "User" should match scopes=["user"] because the role is lowercased.
        assert!(rule_applies_to_role(&["user".to_string()], "User"));
        assert!(rule_applies_to_role(&["user".to_string()], "USER"));
        assert!(rule_applies_to_role(&["user".to_string()], "user"));
        // Non-matching role after lowercasing.
        assert!(!rule_applies_to_role(&["user".to_string()], "Assistant"));
        // Empty scopes => applies to all roles (Go L145-146).
        assert!(rule_applies_to_role(&[], "Anything"));
    }
}

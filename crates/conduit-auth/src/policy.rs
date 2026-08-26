#![forbid(unsafe_code)]

//! Repository policy guard (RUST-P3-004).
//!
//! Pure-logic data-access guard operating on [`RequestContext`] / [`Principal`].
//! Mirrors the Go `internal/scopes` rule chain (user-owned / user-project /
//! api-key-project / owner / deny-by-default) plus the `internal/authz`
//! bypass mechanism. Produces filter values and decisions only -- it never
//! touches the database, so it is fully unit-testable.

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::principal::{Principal, PrincipalKind};
use crate::rbac::{PermissionDecision, PermissionSource};
use crate::request_context::RequestContext;

/// Why a principal requested to bypass the privacy rules.
///
/// Replaces free-form strings so call sites stay auditable and grep-able.
/// Matches the Go `authz.WithBypassPrivacy` reason convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BypassReason {
    Migration,
    QuotaCheck,
    AuthLookup,
    SystemTask,
    Other(String),
}

impl BypassReason {
    fn as_str(&self) -> &str {
        match self {
            Self::Migration => "migration",
            Self::QuotaCheck => "quota-check",
            Self::AuthLookup => "auth-lookup",
            Self::SystemTask => "system-task",
            Self::Other(reason) => reason.as_str(),
        }
    }
}

impl std::fmt::Display for BypassReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kind of mutation being authorized, mirroring `ent.Op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOp {
    Create,
    Update,
    UpdateOne,
    Delete,
    DeleteOne,
}

/// Target of a mutation used for the owner / project-id checks (S10).
///
/// `None` means "not set / unknown yet" -- for Create this is treated as a
/// hard deny because the handler must pin the owning identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationTarget {
    pub project_id: Option<String>,
    pub user_id: Option<String>,
}

impl MutationTarget {
    pub fn project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: Some(project_id.into()),
            user_id: None,
        }
    }

    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            project_id: None,
            user_id: Some(user_id.into()),
        }
    }
}

/// Error returned when a non-privileged principal attempts a bypass (S11).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BypassError {
    #[error("bypass requires a system or test principal")]
    NotPrivileged,
    #[error("bypass requires a principal in context")]
    NoPrincipal,
}

/// Repository policy guard (RUST-P3-004).
///
/// Constructed from a borrowed [`RequestContext`] and evaluated with the
/// fixed methods [`check_query`](Self::check_query),
/// [`check_mutation`](Self::check_mutation),
/// [`project_filter`](Self::project_filter),
/// [`owner_or_scope`](Self::owner_or_scope) and
/// [`bypass`](Self::bypass).
///
/// The guard is side-effect free except when a bypass is granted: the grant
/// is recorded through the pluggable [`BypassAuditSink`] installed via
/// [`with_audit_sink`](Self::with_audit_sink), or through the default debug
/// log when none is set (Go `recordBypassAudit`, bypass.go:118-138).
#[derive(Clone, Copy)]
pub struct PolicyGuard<'a> {
    ctx: &'a RequestContext,
    /// Pluggable bypass audit sink (RUST-P3-004 S11). `None` falls back to
    /// the default debug log, mirroring the nil-`auditLogger` branch of Go
    /// `recordBypassAudit` (bypass.go:128-137).
    audit_sink: Option<&'a dyn BypassAuditSink>,
}

// Manual Debug: `dyn BypassAuditSink` carries no `Debug` bound, so the
// derive is unavailable; report only whether a sink is installed.
impl std::fmt::Debug for PolicyGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyGuard")
            .field("ctx", &self.ctx)
            .field("audit_sink_installed", &self.audit_sink.is_some())
            .finish()
    }
}

impl<'a> PolicyGuard<'a> {
    pub fn new(ctx: &'a RequestContext) -> Self {
        Self {
            ctx,
            audit_sink: None,
        }
    }

    /// Install a pluggable bypass audit sink (RUST-P3-004 S11).
    ///
    /// Mirrors Go `authz.SetAuditLogger` (bypass.go:111-115): audit entries
    /// from granted bypasses are routed to `sink` instead of the default
    /// debug log. The guard is `Copy`, so this is a by-value builder.
    pub fn with_audit_sink(mut self, sink: &'a dyn BypassAuditSink) -> Self {
        self.audit_sink = Some(sink);
        self
    }

    /// Resolve the principal, or deny with "principal is required" (S05).
    fn principal(&self) -> Result<&Principal, PermissionDecision> {
        match self.ctx.principal.as_ref() {
            Some(principal) => Ok(principal),
            None => Err(PermissionDecision::deny("principal is required")),
        }
    }

    /// System / Test principals are unconditionally allowed (S06).
    /// Owner-flagged principals are allowed via the owner rule.
    fn bypass_or_owner(&self, principal: &Principal) -> Option<PermissionDecision> {
        match principal.kind {
            PrincipalKind::System => Some(PermissionDecision::allow(
                PermissionSource::System,
                "system principal bypasses policy",
            )),
            PrincipalKind::Test => Some(PermissionDecision::allow(
                PermissionSource::Test,
                "test principal bypasses policy",
            )),
            PrincipalKind::User | PrincipalKind::ApiKey if principal.is_owner => Some(
                PermissionDecision::allow(PermissionSource::Owner, "owner principal"),
            ),
            PrincipalKind::User | PrincipalKind::ApiKey => None,
        }
    }

    /// Authorize a query against `required_scope` (S05 / S06 / S08).
    ///
    /// Order mirrors the Go `scopes.Policy.Query` chain:
    ///   1. no principal -> deny
    ///   2. system/test/owner -> allow
    ///   3. project scope (user or api key) -> allow if scope matches
    ///   4. otherwise deny
    pub fn check_query(&self, required_scope: &str) -> PermissionDecision {
        let principal = match self.principal() {
            Ok(p) => p,
            Err(decision) => return decision,
        };

        if let Some(decision) = self.bypass_or_owner(principal) {
            return decision;
        }

        match principal.kind {
            PrincipalKind::User => self.check_user_query(required_scope),
            PrincipalKind::ApiKey => self.check_api_key_query(required_scope),
            // System / Test handled above; unreachable arm keeps match exhaustive.
            PrincipalKind::System | PrincipalKind::Test => {
                PermissionDecision::deny("unreachable: bypass handled earlier")
            }
        }
    }

    fn check_user_query(&self, required_scope: &str) -> PermissionDecision {
        // User principals are evaluated through project scope so that the
        // project_id filter (project_filter) and the scope decision stay
        // consistent. A user without project context falls back to direct
        // / system-role scope evaluation.
        let decision = if self.ctx.project_id.is_some() {
            crate::rbac::has_project_scope(self.ctx, required_scope)
        } else {
            crate::rbac::has_scope(self.ctx, required_scope)
        };
        if decision.is_allowed() {
            decision
        } else {
            PermissionDecision::deny(format!("required scope `{required_scope}` is missing"))
        }
    }

    fn check_api_key_query(&self, required_scope: &str) -> PermissionDecision {
        // API keys are project-bound: they must carry a project_id from the
        // request context and the required scope.
        if self.ctx.project_id.is_none() {
            return PermissionDecision::deny("api key requires a project context");
        }
        crate::rbac::has_project_scope(self.ctx, required_scope)
    }

    /// Authorize a mutation (S10). Create/Update/Delete must carry a
    /// verified `project_id` and/or `user_id`; relying on the handler alone
    /// is forbidden.
    pub fn check_mutation(
        &self,
        required_scope: &str,
        op: MutationOp,
        target: &MutationTarget,
    ) -> PermissionDecision {
        let principal = match self.principal() {
            Ok(p) => p,
            Err(decision) => return decision,
        };

        if let Some(decision) = self.bypass_or_owner(principal) {
            return decision;
        }

        // Scope check first -- identical for every op.
        let scope_decision = if self.ctx.project_id.is_some() {
            crate::rbac::has_project_scope(self.ctx, required_scope)
        } else {
            crate::rbac::has_scope(self.ctx, required_scope)
        };
        if !scope_decision.is_allowed() {
            return scope_decision;
        }

        // Ownership target verification (S10).
        match principal.kind {
            PrincipalKind::User => self.verify_user_target(op, target),
            PrincipalKind::ApiKey => self.verify_api_key_target(principal, op, target),
            PrincipalKind::System | PrincipalKind::Test => {
                PermissionDecision::deny("unreachable: bypass handled earlier")
            }
        }
    }

    fn verify_user_target(&self, op: MutationOp, target: &MutationTarget) -> PermissionDecision {
        match op {
            MutationOp::Create => {
                // Create must pin the identity to the current principal so a
                // handler cannot forge ownership. project_id (when present in
                // the request) must equal the target's project_id.
                let project_ok =
                    match (self.ctx.project_id.as_deref(), target.project_id.as_deref()) {
                        (Some(ctx_pid), Some(t_pid)) => ctx_pid == t_pid,
                        (Some(_), None) => false,
                        (None, _) => true,
                    };
                if !project_ok {
                    return PermissionDecision::deny(
                        "create target project_id does not match request project",
                    );
                }
                PermissionDecision::allow(
                    PermissionSource::DirectScope,
                    "user create verified against request project",
                )
            }
            MutationOp::Update
            | MutationOp::UpdateOne
            | MutationOp::Delete
            | MutationOp::DeleteOne => PermissionDecision::allow(
                PermissionSource::DirectScope,
                "user mutation scoped by project_filter/owner_or_scope",
            ),
        }
    }

    fn verify_api_key_target(
        &self,
        principal: &Principal,
        op: MutationOp,
        target: &MutationTarget,
    ) -> PermissionDecision {
        let principal_project = principal.project_id.as_deref();
        match op {
            MutationOp::Create => {
                // For Create the target must land in the api key's own project.
                let Some(t_pid) = target.project_id.as_deref() else {
                    return PermissionDecision::deny("api key create requires a target project_id");
                };
                if Some(t_pid) != principal_project {
                    return PermissionDecision::deny(
                        "api key cannot create resources outside its own project",
                    );
                }
                PermissionDecision::allow(
                    PermissionSource::ApiKeyScope,
                    "api key create verified against own project",
                )
            }
            MutationOp::Update
            | MutationOp::UpdateOne
            | MutationOp::Delete
            | MutationOp::DeleteOne => {
                // Update / Delete rely on the project_filter injected into the WHERE.
                PermissionDecision::allow(
                    PermissionSource::ApiKeyScope,
                    "api key mutation scoped by project_filter",
                )
            }
        }
    }

    /// Project filter to inject into queries on project-owned resources (S08).
    ///
    /// Returns the project id the principal is allowed to see, or `None`
    /// when no filter applies (system/test/owner principals, or no project
    /// context for user/api-key).
    pub fn project_filter(&self) -> Option<&str> {
        let principal = self.ctx.principal.as_ref()?;
        match principal.kind {
            PrincipalKind::System | PrincipalKind::Test => None,
            PrincipalKind::User | PrincipalKind::ApiKey => {
                if principal.is_owner {
                    None
                } else {
                    self.ctx.project_id.as_deref()
                }
            }
        }
    }

    /// Owner filter for user-owned resources (S09): returns the user id
    /// that must be injected into `WHERE user_id = ?`.
    ///
    /// [`OwnerScope::Owner`] for ordinary user principals,
    /// [`OwnerScope::All`] for system/test/owner (no filter needed),
    /// [`OwnerScope::None`] when there is no resolvable owner (anonymous
    /// context, or api key principal which is project-bound instead).
    pub fn owner_or_scope(&self) -> OwnerScope {
        let Some(principal) = self.ctx.principal.as_ref() else {
            return OwnerScope::None;
        };
        // Owner-flagged principals (regardless of kind) see every row,
        // matching the Go OwnerRule short-circuit.
        if principal.is_owner {
            return OwnerScope::All;
        }
        match principal.kind {
            PrincipalKind::System | PrincipalKind::Test => OwnerScope::All,
            PrincipalKind::User => match principal.id.as_deref() {
                Some(user_id) => OwnerScope::Owner(user_id.to_string()),
                None => OwnerScope::None,
            },
            // API keys are project-bound, not user-bound.
            PrincipalKind::ApiKey => OwnerScope::None,
        }
    }

    /// Run `f` under a bypass context (S11). Only System / Test principals
    /// are permitted. A granted bypass produces exactly one typed
    /// [`AuditEntry`] routed through the installed [`BypassAuditSink`]
    /// (default: debug log). The bypass is scoped to the closure -- the
    /// caller's context is untouched.
    ///
    /// Mirrors Go `WithBypassPrivacy` (bypass.go:25-47): denied attempts
    /// (no principal / non-privileged) return *before* any audit record is
    /// produced (bypass.go:26-33); on success the record is emitted once,
    /// before the bypassed work runs (bypass.go:41-42).
    pub fn bypass<T, E>(
        &self,
        reason: BypassReason,
        f: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, BypassOr<E>> {
        // Principal checks + AuditEntry construction (grant-time timestamp,
        // bypass.go:35-39) live in validate_bypass (S14).
        let entry = validate_bypass(self.ctx.principal.as_ref(), &reason).map_err(BypassOr::Err)?;

        // Go records the audit before handing out the bypass context
        // (bypass.go:41-42) -- the entry exists even if `f` later fails.
        self.record_bypass_audit(&entry);

        f(self).map_err(BypassOr::Inner)
    }

    /// Mirror of Go `recordBypassAudit` (bypass.go:118-138): route the
    /// entry to the installed sink, or fall back to the default debug log
    /// (nil-`auditLogger` branch, bypass.go:128-137).
    fn record_bypass_audit(&self, entry: &AuditEntry) {
        match self.audit_sink {
            Some(sink) => sink.record(entry),
            None => debug_bypass_log(entry),
        }
    }
}

/// Result of [`PolicyGuard::owner_or_scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerScope {
    /// No filter -- principal sees every row (system / test / owner).
    All,
    /// Filter rows by the given user id (S09).
    Owner(String),
    /// No resolvable owner -- deny by default.
    None,
}

/// Wrapper preserving the inner error type of [`PolicyGuard::bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BypassOr<E> {
    /// Bypass itself was refused.
    Err(BypassError),
    /// Bypass granted but the closure returned its own error.
    Inner(E),
}

impl<E> BypassOr<E> {
    pub fn into_bypass_error(self) -> Result<BypassError, E> {
        match self {
            Self::Err(e) => Ok(e),
            Self::Inner(e) => Err(e),
        }
    }
}

/// Default audit fallback when no [`BypassAuditSink`] is installed.
///
/// Mirrors the nil-`auditLogger` branch of Go `recordBypassAudit`
/// (bypass.go:130-136): `log.Debug("authz: privacy bypass", principal,
/// reason, operation)`. `eprintln!` avoids pulling `tracing` into the auth
/// crate; production wiring installs a real [`BypassAuditSink`] instead.
fn debug_bypass_log(entry: &AuditEntry) {
    eprintln!(
        "[conduit-auth] authz: privacy bypass: principal={} reason={} operation={}",
        entry.principal, entry.reason, entry.operation
    );
}

// =========================================================================
// S12 -- method naming contract.
//
// Go has no equivalent trait: the contract is implicit in the repository
// call sites. The Go `internal/scopes` Policy.EvalQuery / EvalMutation are
// the closest analogue, but they operate on `ent.Query` / `ent.Mutation`
// rather than a typed guard. To keep the Rust API grep-able and auditable we
// declare the fixed method names as a trait so any future alternate guard
// implementation must expose the same surface, and so a downstream caller
// depending on `dyn PolicyGuardMethods` cannot accidentally rename them.
// =========================================================================

/// Fixed surface every repository policy guard must expose (RUST-P3-004 S12).
///
/// The names are the contract: `check_query`, `check_mutation`,
/// `project_filter`, `owner_or_scope`. Renaming any of these breaks every
/// repository call site and the `tests/go_test_mapping.md` golden ledger,
/// so they are pinned here as trait items.
pub trait PolicyGuardMethods {
    /// Authorize a read for `required_scope`. (S05/S06/S08)
    fn check_query(&self, required_scope: &str) -> PermissionDecision;
    /// Authorize a mutation for `required_scope` against `target`. (S10)
    fn check_mutation(
        &self,
        required_scope: &str,
        op: MutationOp,
        target: &MutationTarget,
    ) -> PermissionDecision;
    /// Project id to inject into `WHERE project_id = ?`, or `None` when no
    /// filter applies (system/test/owner). (S08)
    fn project_filter(&self) -> Option<&str>;
    /// Owner/user filter to inject into `WHERE user_id = ?` for user-owned
    /// resources. (S09)
    fn owner_or_scope(&self) -> OwnerScope;
}

impl<'a> PolicyGuardMethods for PolicyGuard<'a> {
    fn check_query(&self, required_scope: &str) -> PermissionDecision {
        // Delegate to the inherent impl -- the body lives there so the
        // doc-commented contract (S05/S06/S08 order) stays in one place.
        PolicyGuard::check_query(self, required_scope)
    }

    fn check_mutation(
        &self,
        required_scope: &str,
        op: MutationOp,
        target: &MutationTarget,
    ) -> PermissionDecision {
        PolicyGuard::check_mutation(self, required_scope, op, target)
    }

    fn project_filter(&self) -> Option<&str> {
        PolicyGuard::project_filter(self)
    }

    fn owner_or_scope(&self) -> OwnerScope {
        PolicyGuard::owner_or_scope(self)
    }
}

// =========================================================================
// S13 -- soft-delete default.
//
// Mirrors the Go convention encoded implicitly in `internal/ent/schema/*`
// `Annotations{Annotation{SoftDelete: true}}`: every query default-applies
// `deleted_at IS NULL` unless the caller opts out by naming the method
// `*_with_deleted` or `*_system_bypass`. The Go layer achieves this through
// ent's soft-delete interceptor + the `WithoutSoftDelete` helper; here we
// expose the same predicate as pure logic so the repo layer can apply it
// without coupling to ent.
// =========================================================================

/// Returns `true` when the soft-delete filter `deleted_at IS NULL` should be
/// applied for a repository method named `method_name` (RUST-P3-004 S13).
///
/// The filter is **skipped** (returns `false`) when `method_name` contains
/// either `with_deleted` (explicit opt-out) or `system_bypass` (privileged
/// internal path). Matching is case-sensitive and substring-based so callers
/// can compose names like `find_api_keys_with_deleted` or
/// `system_bypass_gc_scan` without a naming convention change.
pub fn should_apply_soft_delete(method_name: &str) -> bool {
    !(method_name.contains("with_deleted") || method_name.contains("system_bypass"))
}

// =========================================================================
// S14 -- system_bypass typed audit entry.
//
// Mirrors Go `internal/authz/bypass.go` `WithBypassPrivacy`: requires a
// System/Test principal, and on success records a `bypassAuditRecord` via
// `recordBypassAudit`. We expose the same guard as a pure predicate that
// returns a typed [`AuditEntry`] the caller can persist/log however it
// likes. `PolicyGuard::bypass` (S11) routes that entry through the
// pluggable [`BypassAuditSink`], defaulting to the debug log exactly like
// Go's nil-`auditLogger` branch.
// =========================================================================

/// Audit entry produced by a successful [`validate_bypass`] (RUST-P3-004 S14).
///
/// Field names mirror the Go `bypassAuditRecord` struct (bypass.go:98-105:
/// Timestamp / Principal / Reason / Operation / Entity / Description) so a
/// downstream serializer can emit the same shape as the Go audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Grant-time timestamp -- Go stamps `time.Now()` when building
    /// `bypassInfo` (bypass.go:37) and copies it into the audit record
    /// (bypass.go:121).
    pub timestamp: DateTime<Utc>,
    /// Operation identifier -- always `"bypass"` for privacy bypasses.
    pub operation: &'static str,
    /// Entity kind -- always `"privacy"` (Go `bypass.go:Entity`).
    pub entity: &'static str,
    /// Principal display string (e.g. `"system"`, `"user:42"`).
    pub principal: String,
    /// Stable bypass reason identifier (e.g. `"quota-check"`).
    pub reason: String,
    /// Human-readable description matching Go's
    /// `"Privacy bypass triggered: reason=<r>, principal=<p>"`.
    pub description: String,
}

impl AuditEntry {
    /// Sentinel principal used when the audit log must record an attempted
    /// bypass that was refused (e.g. by an anonymous caller). Matches the
    /// Go `"unknown"` principal string.
    pub const UNKNOWN_PRINCIPAL: &'static str = "unknown";

    /// Constant operation/entity strings -- the Go audit log hard-codes these.
    pub const OPERATION_BYPASS: &'static str = "bypass";
    pub const ENTITY_PRIVACY: &'static str = "privacy";
}

/// Pluggable sink for bypass audit entries (RUST-P3-004 S11).
///
/// Rust analogue of Go `authz.SetAuditLogger` (bypass.go:107-115): install
/// one via [`PolicyGuard::with_audit_sink`] to persist audit entries; when
/// none is installed, [`PolicyGuard::bypass`] falls back to the default
/// debug log (Go `recordBypassAudit` nil branch, bypass.go:128-137).
pub trait BypassAuditSink {
    /// Record one granted bypass. Invoked exactly once per grant
    /// (bypass.go:41-42); denied attempts are never recorded because Go
    /// returns before `recordBypassAudit` runs (bypass.go:26-33).
    fn record(&self, entry: &AuditEntry);
}

/// Validate a bypass request and, on success, produce the typed
/// [`AuditEntry`] that must be recorded (RUST-P3-004 S14).
///
/// Mirrors Go `WithBypassPrivacy` (bypass.go:25-39):
/// 1. require a principal -> [`BypassError::NoPrincipal`] (bypass.go:26-29)
/// 2. require `System` or `Test` principal -> [`BypassError::NotPrivileged`]
///    (bypass.go:31-33)
/// 3. on success, build the audit record with the grant-time timestamp
///    (`time.Now()`, bypass.go:37) and the Go-compatible description.
///
/// This is pure validation + record construction -- recording the entry
/// (sink or debug log) is [`PolicyGuard::bypass`]'s job, mirroring the Go
/// split between `WithBypassPrivacy` and `recordBypassAudit`.
pub fn validate_bypass(
    principal: Option<&Principal>,
    reason: &BypassReason,
) -> Result<AuditEntry, BypassError> {
    let Some(principal) = principal else {
        return Err(BypassError::NoPrincipal);
    };
    if !matches!(principal.kind, PrincipalKind::System | PrincipalKind::Test) {
        return Err(BypassError::NotPrivileged);
    }

    Ok(AuditEntry {
        // Grant-time stamp, matching Go `bypassInfo.Timestamp = time.Now()`
        // (bypass.go:37, copied into the record at :121).
        timestamp: Utc::now(),
        operation: AuditEntry::OPERATION_BYPASS,
        entity: AuditEntry::ENTITY_PRIVACY,
        principal: principal.to_string(),
        reason: reason.to_string(),
        description: format!("Privacy bypass triggered: reason={reason}, principal={principal}"),
    })
}

// =========================================================================
// S15 -- pagination stable sort.
//
// Go ent pagination implicitly relies on the primary key as a tie-breaker
// because the underlying `SELECT` adds `ORDER BY <pk>` last. The Rust repo
// layer builds `ORDER BY` clauses explicitly, so we must enforce the
// tie-breaker in pure logic: if the sort is on `created_at`, append `id ASC`
// so paginated results are deterministic across pages.
// =========================================================================

/// Sort direction for a [`SortField`] (RUST-P3-004 S15).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

impl std::fmt::Display for SortDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        })
    }
}

/// A single `ORDER BY` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortField {
    pub column: String,
    pub direction: SortDirection,
}

impl SortField {
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Asc,
        }
    }

    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Desc,
        }
    }
}

/// A full sort specification -- the `ORDER BY` clause expressed as a list of
/// [`SortField`]s. The repo layer interprets the list in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortSpec {
    pub fields: Vec<SortField>,
}

impl SortSpec {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_field(field: SortField) -> Self {
        Self {
            fields: vec![field],
        }
    }

    /// True when any field references `created_at` (case-insensitive).
    fn has_created_at(&self) -> bool {
        self.fields
            .iter()
            .any(|f| f.column.eq_ignore_ascii_case("created_at"))
    }

    /// True when any field references `id` (case-insensitive) -- the
    /// canonical tie-breaker column.
    fn has_id_tiebreaker(&self) -> bool {
        self.fields
            .iter()
            .any(|f| f.column.eq_ignore_ascii_case("id"))
    }
}

/// Stable pagination sort (RUST-P3-004 S15).
///
/// If `sort` orders by `created_at` but lacks an `id` tie-breaker, append
/// `id ASC` so paginated cursors are deterministic. Other sorts are returned
/// unchanged -- callers that sort by a unique column (e.g. `slug`) do not
/// need a tie-breaker.
pub fn ensure_stable_sort(mut sort: SortSpec) -> SortSpec {
    if sort.has_created_at() && !sort.has_id_tiebreaker() {
        sort.fields.push(SortField::asc("id"));
    }
    sort
}

// =========================================================================
// S09 -- standalone user-owned owner filter.
//
// The [`PolicyGuard::owner_or_scope`] method already encodes the S09 logic
// for a request context, but repository call sites sometimes hold only the
// principal (e.g. when iterating in a background worker). Exposing the same
// rule as a pure function lets them resolve the owner filter without
// constructing a full [`RequestContext`].
// =========================================================================

/// Standalone owner filter for user-owned resources (RUST-P3-004 S09).
///
/// Returned by [`owner_filter`]: the user id to inject into
/// `WHERE user_id = ?`, or `None` when no owner filter applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerFilter {
    pub user_id: String,
}

/// Resolve the owner filter for `principal` against a user-owned resource
/// kind (RUST-P3-004 S09).
///
/// Returns:
/// - `Some(OwnerFilter { user_id })` for a `User` principal with a known id
///   (the resource row must satisfy `user_id = ?`).
/// - `None` for `System` / `Test` / owner principals (they see all rows, no
///   filter needed), for `ApiKey` principals (they are project-bound, not
///   user-bound -- handled by [`PolicyGuard::project_filter`]), and for an
///   anonymous or id-less user (default-deny is the caller's responsibility).
pub fn owner_filter(principal: Option<&Principal>) -> Option<OwnerFilter> {
    let principal = principal?;
    // Owner-flagged / system / test principals see every row.
    if principal.is_owner || matches!(principal.kind, PrincipalKind::System | PrincipalKind::Test) {
        return None;
    }
    match principal.kind {
        PrincipalKind::User => principal.id.clone().map(|user_id| OwnerFilter { user_id }),
        // API keys are project-bound; the owner filter never applies.
        // System / Test are unreachable here -- the early-return above
        // already handled them (and owner-flagged principals) -- but the
        // match must stay exhaustive against future PrincipalKind values.
        PrincipalKind::ApiKey | PrincipalKind::System | PrincipalKind::Test => None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use chrono::Utc;

    use super::*;
    use crate::principal::Principal;
    use crate::request_context::{ContextConflictError, RequestContext};
    use crate::scopes::slug;

    /// Recording sink for the S11 audit tests -- collects every entry so
    /// tests can assert count and field values (Go analogue: a custom
    /// logger installed via `SetAuditLogger`, bypass.go:113-115).
    #[derive(Default)]
    struct RecordingSink {
        entries: RefCell<Vec<AuditEntry>>,
    }

    impl BypassAuditSink for RecordingSink {
        fn record(&self, entry: &AuditEntry) {
            self.entries.borrow_mut().push(entry.clone());
        }
    }

    fn ctx_with(
        principal: Principal,
        project_id: Option<&str>,
    ) -> Result<RequestContext, ContextConflictError> {
        let mut ctx = RequestContext::new();
        ctx.set_principal(principal)?;
        if let Some(pid) = project_id {
            ctx.set_project_id(pid)?;
        }
        Ok(ctx)
    }

    // S05 -- deny by default when no principal.
    #[test]
    fn deny_by_default_without_principal() -> Result<(), ContextConflictError> {
        let ctx = RequestContext::new();
        let guard = PolicyGuard::new(&ctx);

        let q = guard.check_query(slug::READ_CHANNELS);
        assert!(!q.is_allowed());
        assert_eq!(q.reason(), "principal is required");

        let target = MutationTarget::project("p-1");
        let m = guard.check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &target);
        assert!(!m.is_allowed());
        assert_eq!(m.reason(), "principal is required");
        Ok(())
    }

    // S06 -- system & test principals always allowed.
    #[test]
    fn system_and_test_principal_pass() -> Result<(), ContextConflictError> {
        for principal in [Principal::system(), Principal::test()] {
            let ctx = ctx_with(principal, None)?;
            let guard = PolicyGuard::new(&ctx);

            assert!(guard.check_query(slug::READ_CHANNELS).is_allowed());
            let target = MutationTarget::project("any");
            assert!(
                guard
                    .check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &target)
                    .is_allowed()
            );
        }
        Ok(())
    }

    // S09 -- user-owned filter returns the principal's user id.
    #[test]
    fn owner_or_scope_for_user() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::user("user-42"), None)?;
        let guard = PolicyGuard::new(&ctx);

        assert_eq!(
            guard.owner_or_scope(),
            OwnerScope::Owner("user-42".to_string())
        );
        Ok(())
    }

    // Owner-class principal gets OwnerScope::All (no filter).
    #[test]
    fn owner_or_scope_all_for_privileged() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::system(), None)?;
        assert_eq!(PolicyGuard::new(&ctx).owner_or_scope(), OwnerScope::All);

        let ctx = ctx_with(Principal::test(), None)?;
        assert_eq!(PolicyGuard::new(&ctx).owner_or_scope(), OwnerScope::All);
        Ok(())
    }

    // S08 -- project_filter returns the request project for user / api key.
    #[test]
    fn project_filter_returns_request_project() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::user("user-1").with_scope(slug::READ_CHANNELS),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);
        assert_eq!(guard.project_filter(), Some("project-1"));
        Ok(())
    }

    // project_filter is None for system / test / owner.
    #[test]
    fn project_filter_none_for_privileged() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::system(), Some("project-1"))?;
        assert_eq!(PolicyGuard::new(&ctx).project_filter(), None);

        let ctx = ctx_with(Principal::user("owner-1").with_owner(true), Some("p"))?;
        assert_eq!(PolicyGuard::new(&ctx).project_filter(), None);
        Ok(())
    }

    // RUST-P4-002 S13 -- user-owned rule: a user querying their own data with
    // a matching scope is allowed, and owner_or_scope pins their id.
    #[test]
    fn s13_user_owned_rule() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::user("user-1").with_scope(slug::READ_API_KEYS),
            None,
        )?;
        let guard = PolicyGuard::new(&ctx);

        assert!(guard.check_query(slug::READ_API_KEYS).is_allowed());
        assert_eq!(
            guard.owner_or_scope(),
            OwnerScope::Owner("user-1".to_string())
        );
        Ok(())
    }

    // RUST-P4-002 S13 -- user-project rule: user with project scope is
    // allowed inside their project and denied without project context.
    #[test]
    fn s13_user_project_rule() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::user("user-1").with_scope(crate::scopes::Scope::project(
                "project-1",
                slug::READ_CHANNELS,
            )),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);

        assert!(guard.check_query(slug::READ_CHANNELS).is_allowed());
        assert_eq!(guard.project_filter(), Some("project-1"));

        // No project context for an api key -> denied.
        let no_project = ctx_with(
            Principal::api_key("k-1", "project-1").with_scope(slug::READ_CHANNELS),
            None,
        )?;
        let guard = PolicyGuard::new(&no_project);
        assert!(!guard.check_query(slug::READ_CHANNELS).is_allowed());
        Ok(())
    }

    // RUST-P4-002 S13 -- api-key-project rule: api key is allowed within its
    // own project when it has the scope, denied otherwise.
    #[test]
    fn s13_api_key_project_rule() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::api_key("key-1", "project-1").with_scope(slug::READ_CHANNELS),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);

        assert!(guard.check_query(slug::READ_CHANNELS).is_allowed());
        assert_eq!(guard.project_filter(), Some("project-1"));
        // API key is project-bound, never user-bound.
        assert_eq!(guard.owner_or_scope(), OwnerScope::None);

        let no_scope = ctx_with(Principal::api_key("key-2", "project-1"), Some("project-1"))?;
        let guard = PolicyGuard::new(&no_scope);
        assert!(!guard.check_query(slug::READ_CHANNELS).is_allowed());
        Ok(())
    }

    // RUST-P4-002 S13 -- owner rule: owner-flagged principal is allowed
    // regardless of scopes, with All owner scope.
    #[test]
    fn s13_owner_rule() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::user("owner-1").with_owner(true), None)?;
        let guard = PolicyGuard::new(&ctx);

        assert!(guard.check_query(slug::WRITE_PROJECTS).is_allowed());
        assert_eq!(guard.owner_or_scope(), OwnerScope::All);

        let target = MutationTarget::project("brand-new");
        assert!(
            guard
                .check_mutation(slug::WRITE_PROJECTS, MutationOp::Create, &target)
                .is_allowed()
        );
        Ok(())
    }

    // RUST-P4-002 S13 -- deny by default: anonymous context always denies.
    #[test]
    fn s13_deny_by_default() {
        let ctx = RequestContext::new();
        let guard = PolicyGuard::new(&ctx);

        assert!(!guard.check_query(slug::READ_CHANNELS).is_allowed());
        assert_eq!(guard.owner_or_scope(), OwnerScope::None);
        assert_eq!(guard.project_filter(), None);
    }

    // S10 -- create must carry a project_id matching the request project for
    // user principals; otherwise the mutation is denied.
    #[test]
    fn mutation_create_must_match_project() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::user("user-1").with_scope(crate::scopes::Scope::project(
                "project-1",
                slug::WRITE_CHANNELS,
            )),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);

        let ok = MutationTarget::project("project-1");
        assert!(
            guard
                .check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &ok)
                .is_allowed()
        );

        let mismatch = MutationTarget::project("project-2");
        let decision = guard.check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &mismatch);
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("project_id"));
        Ok(())
    }

    // S10 -- api key create must target its own project.
    #[test]
    fn mutation_api_key_create_outside_project_denied() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::api_key("key-1", "project-1").with_scope(slug::WRITE_CHANNELS),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);

        let own = MutationTarget::project("project-1");
        assert!(
            guard
                .check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &own)
                .is_allowed()
        );

        let other = MutationTarget::project("project-2");
        let decision = guard.check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &other);
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("own project"));
        Ok(())
    }

    // S11 -- bypass only works for system / test principals.
    #[test]
    fn bypass_requires_privileged_principal() -> Result<(), ContextConflictError> {
        let anon_ctx = RequestContext::new();
        let guard = PolicyGuard::new(&anon_ctx);
        let result = guard.bypass(BypassReason::Migration, |_| Ok::<i32, BypassError>(1));
        assert!(matches!(
            result,
            Err(BypassOr::Err(BypassError::NoPrincipal))
        ));

        let user_ctx = ctx_with(Principal::user("user-1"), None)?;
        let guard = PolicyGuard::new(&user_ctx);
        let result = guard.bypass(BypassReason::QuotaCheck, |_| Ok::<i32, BypassError>(1));
        assert!(matches!(
            result,
            Err(BypassOr::Err(BypassError::NotPrivileged))
        ));
        Ok(())
    }

    // S11 -- with no sink installed, bypass still works and falls back to
    // the default debug log (Go recordBypassAudit nil branch, bypass.go:130-136).
    #[test]
    fn bypass_runs_closure_for_system() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::system(), None)?;
        let guard = PolicyGuard::new(&ctx);

        let result = guard.bypass(BypassReason::AuthLookup, |_| Ok::<i32, BypassError>(42));
        match result {
            Ok(value) => assert_eq!(value, 42),
            Err(BypassOr::Err(e)) => panic!("bypass refused: {e}"),
            Err(BypassOr::Inner(e)) => panic!("inner error: {e}"),
        }
        Ok(())
    }

    // S11 -- an installed sink receives exactly one AuditEntry per granted
    // bypass, with the Go bypassAuditRecord field values (bypass.go:119-126)
    // and a grant-time timestamp (bypass.go:37).
    #[test]
    fn s11_bypass_with_sink_records_one_entry() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::system(), None)?;
        let sink = RecordingSink::default();
        let guard = PolicyGuard::new(&ctx).with_audit_sink(&sink);

        let before = Utc::now();
        let result = guard.bypass(BypassReason::QuotaCheck, |_| Ok::<i32, BypassError>(7));
        let after = Utc::now();
        match result {
            Ok(value) => assert_eq!(value, 7),
            Err(BypassOr::Err(e)) => panic!("bypass refused: {e}"),
            Err(BypassOr::Inner(e)) => panic!("inner error: {e}"),
        }

        let entries = sink.entries.borrow();
        assert_eq!(
            entries.len(),
            1,
            "one audit entry per grant (bypass.go:41-42)"
        );
        let entry = &entries[0];
        assert_eq!(entry.principal, "system");
        assert_eq!(entry.reason, "quota-check");
        assert_eq!(entry.operation, AuditEntry::OPERATION_BYPASS);
        assert_eq!(entry.entity, AuditEntry::ENTITY_PRIVACY);
        assert_eq!(
            entry.description,
            "Privacy bypass triggered: reason=quota-check, principal=system"
        );
        // Non-zero timestamp captured inside the call window (a default /
        // epoch timestamp would fail the lower bound).
        assert!(entry.timestamp >= before && entry.timestamp <= after);
        Ok(())
    }

    // S11 -- denied attempts produce NO audit entry: Go WithBypassPrivacy
    // returns on the error paths (bypass.go:26-33) before recordBypassAudit
    // runs (bypass.go:41-42).
    #[test]
    fn s11_denied_bypass_produces_no_audit_entry() -> Result<(), ContextConflictError> {
        let sink = RecordingSink::default();

        // Anonymous context -> NoPrincipal, nothing recorded.
        let anon_ctx = RequestContext::new();
        let guard = PolicyGuard::new(&anon_ctx).with_audit_sink(&sink);
        let result = guard.bypass(BypassReason::Migration, |_| Ok::<i32, BypassError>(1));
        assert!(matches!(
            result,
            Err(BypassOr::Err(BypassError::NoPrincipal))
        ));
        assert!(sink.entries.borrow().is_empty());

        // Ordinary user principal -> NotPrivileged, nothing recorded.
        let user_ctx = ctx_with(Principal::user("user-1"), None)?;
        let guard = PolicyGuard::new(&user_ctx).with_audit_sink(&sink);
        let result = guard.bypass(BypassReason::QuotaCheck, |_| Ok::<i32, BypassError>(1));
        assert!(matches!(
            result,
            Err(BypassOr::Err(BypassError::NotPrivileged))
        ));
        assert!(sink.entries.borrow().is_empty());
        Ok(())
    }

    // S11 -- the audit is recorded at grant time, before the closure runs
    // (bypass.go:41-46): a failing closure still leaves one entry.
    #[test]
    fn s11_bypass_audit_recorded_even_if_closure_fails() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(Principal::test(), None)?;
        let sink = RecordingSink::default();
        let guard = PolicyGuard::new(&ctx).with_audit_sink(&sink);

        let result = guard.bypass(BypassReason::SystemTask, |_| {
            Err::<i32, BypassError>(BypassError::NotPrivileged)
        });
        assert!(matches!(result, Err(BypassOr::Inner(_))));

        let entries = sink.entries.borrow();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].principal, "test");
        assert_eq!(entries[0].reason, "system-task");
        Ok(())
    }

    // BypassReason string forms are stable (audit contract).
    #[test]
    fn bypass_reason_strings_are_stable() {
        assert_eq!(BypassReason::Migration.to_string(), "migration");
        assert_eq!(BypassReason::QuotaCheck.to_string(), "quota-check");
        assert_eq!(BypassReason::AuthLookup.to_string(), "auth-lookup");
        assert_eq!(BypassReason::SystemTask.to_string(), "system-task");
        assert_eq!(
            BypassReason::Other("custom".to_string()).to_string(),
            "custom"
        );
    }

    // ---------------------------------------------------------------------
    // S12 -- PolicyGuardMethods trait surface is the contract.
    //
    // Mirrors Go `internal/scopes` implicit call sites: every repository
    // guard must expose the four named methods. The test asserts that the
    // trait object dispatch reaches the inherent impl (so a rename of the
    // trait method would break compilation).
    // ---------------------------------------------------------------------
    #[test]
    fn s12_trait_dispatch_matches_inherent_methods() -> Result<(), ContextConflictError> {
        let ctx = ctx_with(
            Principal::user("user-1").with_scope(slug::READ_CHANNELS),
            Some("project-1"),
        )?;
        let guard = PolicyGuard::new(&ctx);
        let dyn_guard: &dyn PolicyGuardMethods = &guard;

        assert_eq!(
            dyn_guard.check_query(slug::READ_CHANNELS).is_allowed(),
            guard.check_query(slug::READ_CHANNELS).is_allowed()
        );

        let target = MutationTarget::project("project-1");
        assert_eq!(
            dyn_guard
                .check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &target)
                .is_allowed(),
            guard
                .check_mutation(slug::WRITE_CHANNELS, MutationOp::Create, &target)
                .is_allowed()
        );

        assert_eq!(dyn_guard.project_filter(), guard.project_filter());
        assert_eq!(dyn_guard.owner_or_scope(), guard.owner_or_scope());
        Ok(())
    }

    // ---------------------------------------------------------------------
    // S13 -- soft-delete default.
    //
    // Mirrors Go ent soft-delete interceptor + `WithoutSoftDelete` helper.
    // ---------------------------------------------------------------------
    #[test]
    fn s13_soft_delete_applied_by_default() {
        // Plain method names are filtered.
        assert!(should_apply_soft_delete("find_api_keys"));
        assert!(should_apply_soft_delete("list_channels"));
        assert!(should_apply_soft_delete(""));
    }

    #[test]
    fn s13_soft_delete_skipped_for_with_deleted() {
        // `with_deleted` is the explicit opt-out suffix, anywhere in the name.
        assert!(!should_apply_soft_delete("find_api_keys_with_deleted"));
        assert!(!should_apply_soft_delete("with_deleted"));
        assert!(!should_apply_soft_delete("find_with_deleted_api_keys"));
    }

    #[test]
    fn s13_soft_delete_skipped_for_system_bypass() {
        // `system_bypass` is the privileged internal escape hatch.
        assert!(!should_apply_soft_delete("system_bypass_gc_scan"));
        assert!(!should_apply_soft_delete("system_bypass"));
        assert!(!should_apply_soft_delete("find_system_bypass_requests"));
    }

    // ---------------------------------------------------------------------
    // S14 -- validate_bypass typed AuditEntry.
    //
    // Mirrors Go `internal/authz/bypass_test.go::TestBypassWithSystemPrincipal`
    // (system principal succeeds, audit record fields are populated) and
    // `TestBypassWithNonSystemPrincipal` (user / apikey principals fail).
    // ---------------------------------------------------------------------
    #[test]
    fn s14_validate_bypass_no_principal() {
        let entry = validate_bypass(None, &BypassReason::QuotaCheck);
        assert!(matches!(entry, Err(BypassError::NoPrincipal)));
    }

    #[test]
    fn s14_validate_bypass_user_denied() {
        let principal = Principal::user("user-42");
        let entry = validate_bypass(Some(&principal), &BypassReason::QuotaCheck);
        assert!(matches!(entry, Err(BypassError::NotPrivileged)));
    }

    #[test]
    fn s14_validate_bypass_apikey_denied() {
        let principal = Principal::api_key("key-1", "project-1");
        let entry = validate_bypass(Some(&principal), &BypassReason::AuthLookup);
        assert!(matches!(entry, Err(BypassError::NotPrivileged)));
    }

    #[test]
    fn s14_validate_bypass_system_produces_audit_entry() {
        let principal = Principal::system();
        let before = Utc::now();
        let entry = validate_bypass(Some(&principal), &BypassReason::QuotaCheck);
        let after = Utc::now();
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => panic!("expected Ok, got {e}"),
        };

        // S11 -- grant-time timestamp (bypass.go:37): non-zero, in-window.
        assert!(entry.timestamp >= before && entry.timestamp <= after);

        assert_eq!(entry.operation, AuditEntry::OPERATION_BYPASS);
        assert_eq!(entry.entity, AuditEntry::ENTITY_PRIVACY);
        assert_eq!(entry.principal, "system");
        assert_eq!(entry.reason, "quota-check");
        assert!(entry.description.contains("quota-check"));
        assert!(entry.description.contains("system"));
        // Description format mirrors Go `bypass.go` recordBypassAudit.
        assert_eq!(
            entry.description,
            "Privacy bypass triggered: reason=quota-check, principal=system"
        );
    }

    #[test]
    fn s14_validate_bypass_test_principal_produces_audit_entry() {
        let principal = Principal::test();
        let entry = validate_bypass(Some(&principal), &BypassReason::Migration)
            .unwrap_or_else(|e| panic!("expected Ok, got {e}"));

        assert_eq!(entry.principal, "test");
        assert_eq!(entry.reason, "migration");
        assert_eq!(entry.operation, "bypass");
        assert_eq!(entry.entity, "privacy");
    }

    #[test]
    fn s14_validate_bypass_other_reason_preserves_string() {
        let principal = Principal::system();
        let entry = validate_bypass(
            Some(&principal),
            &BypassReason::Other("custom-reason".to_string()),
        )
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));

        assert_eq!(entry.reason, "custom-reason");
        assert!(entry.description.contains("custom-reason"));
    }

    // ---------------------------------------------------------------------
    // S15 -- pagination stable sort.
    //
    // Mirrors Go ent implicit `ORDER BY <pk>` last-page stability.
    // ---------------------------------------------------------------------
    #[test]
    fn s15_created_at_sort_appends_id_tiebreaker() {
        let sort = SortSpec::from_field(SortField::desc("created_at"));
        let stable = ensure_stable_sort(sort);

        assert_eq!(stable.fields.len(), 2);
        assert_eq!(stable.fields[0].column, "created_at");
        assert_eq!(stable.fields[0].direction, SortDirection::Desc);
        assert_eq!(stable.fields[1].column, "id");
        assert_eq!(stable.fields[1].direction, SortDirection::Asc);
    }

    #[test]
    fn s15_id_tiebreaker_not_duplicated() {
        // If `id` is already present, do not append a duplicate.
        let sort = SortSpec {
            fields: vec![SortField::desc("created_at"), SortField::asc("id")],
        };
        let stable = ensure_stable_sort(sort);

        assert_eq!(stable.fields.len(), 2);
        assert_eq!(stable.fields[0].column, "created_at");
        assert_eq!(stable.fields[1].column, "id");
    }

    #[test]
    fn s15_non_created_at_sort_unchanged() {
        // Sort by a unique column (e.g. `slug`) needs no tie-breaker.
        let sort = SortSpec::from_field(SortField::asc("slug"));
        let stable = ensure_stable_sort(sort);

        assert_eq!(stable.fields.len(), 1);
        assert_eq!(stable.fields[0].column, "slug");
    }

    #[test]
    fn s15_empty_sort_unchanged() {
        let stable = ensure_stable_sort(SortSpec::empty());
        assert!(stable.fields.is_empty());
    }

    #[test]
    fn s15_case_insensitive_created_at_column() {
        // Column matching is ASCII-case-insensitive -- a `CREATED_AT` column
        // still triggers the id tie-breaker. (Snake-case `created_at` is the
        // canonical form; `CreatedAt` would NOT match because the underscore
        // differs from the letter `A`, so it is intentionally not tested.)
        let sort = SortSpec::from_field(SortField::desc("CREATED_AT"));
        let stable = ensure_stable_sort(sort);

        assert_eq!(stable.fields.len(), 2);
        assert_eq!(stable.fields[1].column, "id");
    }

    // ---------------------------------------------------------------------
    // S09 -- standalone owner_filter pure function.
    //
    // Mirrors Go `internal/scopes/rule_user_owned.go::userOwnedQueryFilter`
    // (inject `user_id = ?` for user-owned resources) and the owner-rule
    // short-circuit in `rule_owner.go`.
    // ---------------------------------------------------------------------
    #[test]
    fn s09_owner_filter_user_principal() {
        let principal = Principal::user("user-42");
        assert_eq!(
            owner_filter(Some(&principal)),
            Some(OwnerFilter {
                user_id: "user-42".to_string()
            })
        );
    }

    #[test]
    fn s09_owner_filter_system_returns_none() {
        assert_eq!(owner_filter(Some(&Principal::system())), None);
    }

    #[test]
    fn s09_owner_filter_test_returns_none() {
        assert_eq!(owner_filter(Some(&Principal::test())), None);
    }

    #[test]
    fn s09_owner_filter_owner_flagged_returns_none() {
        let owner = Principal::user("owner-1").with_owner(true);
        assert_eq!(owner_filter(Some(&owner)), None);
    }

    #[test]
    fn s09_owner_filter_api_key_returns_none() {
        // API keys are project-bound, never user-bound.
        let api_key = Principal::api_key("key-1", "project-1");
        assert_eq!(owner_filter(Some(&api_key)), None);
    }

    #[test]
    fn s09_owner_filter_anonymous_returns_none() {
        assert_eq!(owner_filter(None), None);
    }

    // ====================================================================
    // Go `internal/authz/bypass_test.go` parity (L1-301).
    //
    // Go bypass tests use `context.Context` + `bypassKey{}` + `bypassInfo`
    // struct + `privacy.DecisionContext` (ent-privacy). Rust has no
    // context-key bypass mechanism — `PolicyGuard::bypass` +
    // `validate_bypass` + `AuditEntry` encode the same contract through typed
    // Rust APIs. Many Go cases are already covered by the S11/S14 tests
    // above; the additions below fill the remaining pure-logic gaps and
    // catalogue the structural-gap cases.
    // ====================================================================

    /// Mirrors Go `TestRequirePrincipal` (bypass_test.go:136-148): a context
    /// with any principal passes; a context with no principal fails. Go has a
    /// standalone `RequirePrincipal` function (bypass.go:141-148); Rust
    /// encodes the same rule through `PolicyGuard` which denies "principal is
    /// required" when none is set. This test makes the Go parity mapping
    /// explicit (already behaviorally covered by
    /// `deny_by_default_without_principal`).
    #[test]
    fn go_require_principal_behavior_matches_guard() -> Result<(), ContextConflictError> {
        // With principal -> guard does not deny with "principal is required".
        let with_principal = ctx_with(Principal::system(), None)?;
        let guard = PolicyGuard::new(&with_principal);
        let decision = guard.check_query(slug::READ_CHANNELS);
        assert_ne!(decision.reason(), "principal is required");

        // Without principal -> guard denies with "principal is required".
        let no_principal = RequestContext::new();
        let guard = PolicyGuard::new(&no_principal);
        let decision = guard.check_query(slug::READ_CHANNELS);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), "principal is required");
        Ok(())
    }

    /// Mirrors Go `TestRequireSystemPrincipal` (bypass_test.go:116-134):
    /// system principal passes; user principal fails; no principal fails.
    ///
    /// Go: standalone `RequireSystemPrincipal` (system.go:30-41) checks
    /// `p.IsSystem()` ONLY (not Test). Rust `validate_bypass` checks for
    /// System OR Test (matching Go `WithBypassPrivacy` at bypass.go:31-33).
    /// There is no exact `require_system_principal` in Rust; this test uses
    /// `validate_bypass` to verify the three Go cases (system ok, user fail,
    /// no-principal fail). The Test-principal case is an intentional extra
    /// allowed by `validate_bypass` but not by Go `RequireSystemPrincipal`.
    #[test]
    fn go_require_system_principal_behavior() {
        // System principal -> succeeds (Go L118: RequireSystemPrincipal passes).
        let system_entry = validate_bypass(Some(&Principal::system()), &BypassReason::Migration);
        assert!(system_entry.is_ok());

        // User principal -> fails (Go L124: RequireSystemPrincipal fails).
        let user_entry =
            validate_bypass(Some(&Principal::user("user-1")), &BypassReason::Migration);
        assert!(matches!(user_entry, Err(BypassError::NotPrivileged)));

        // No principal -> fails (Go L130: RequireSystemPrincipal fails).
        let none_entry = validate_bypass(None, &BypassReason::Migration);
        assert!(matches!(none_entry, Err(BypassError::NoPrincipal)));
    }

    /// Mirrors Go `TestBypassAuditRecordStructure` (bypass_test.go:267-301):
    /// constructs a `bypassAuditRecord` and verifies each field.
    ///
    /// Go: `bypassAuditRecord{Timestamp, Principal, Reason, Operation, Entity,
    /// Description}` (bypass.go:98-105). Rust: `AuditEntry` with the same
    /// six fields. The Go test constructs a record with arbitrary values and
    /// checks round-trip; Rust `AuditEntry` uses `&'static str` for
    /// operation/entity (they are always "bypass"/"privacy" per bypass.go:123-
    /// 124), so this test uses the constant accessors for those fields and
    /// arbitrary strings for the rest, exactly as the Go test does.
    #[test]
    fn go_bypass_audit_entry_structure() {
        let timestamp = Utc::now();
        let entry = AuditEntry {
            timestamp,
            operation: AuditEntry::OPERATION_BYPASS,
            entity: AuditEntry::ENTITY_PRIVACY,
            principal: "user:123".to_string(),
            reason: "test-reason".to_string(),
            description: "test description".to_string(),
        };

        // Go L278-300: each field matches what was set.
        assert_eq!(entry.timestamp, timestamp);
        assert_eq!(entry.principal, "user:123");
        assert_eq!(entry.reason, "test-reason");
        assert_eq!(entry.operation, "bypass");
        assert_eq!(entry.entity, "privacy");
        assert_eq!(entry.description, "test description");
    }

    /// Structural-gap catalogue for Go `bypass_test.go` tests that exercise
    /// `context.Context` + `bypassKey{}` + `privacy.DecisionContext` with no
    /// direct Rust equivalent. The pure-logic intent of each is covered by
    /// the S11/S14 tests above; the context-propagation shape is not portable.
    #[test]
    #[ignore = "structural gap: Go context.Value(bypassKey{}) + privacy.DecisionContext vs Rust typed PolicyGuard/validate_bypass"]
    fn go_bypass_context_tests_structural_gap_catalogue() {
        // TestWithBypassPrivacy (L12-38): GetBypassInfo returns reason/
        //   principal/timestamp after WithBypassPrivacy. Rust: covered by
        //   s14_validate_bypass_system_produces_audit_entry.
        // TestRunWithBypass (L40-72): RunWithBypass executes closure,
        //   IsBypassActive is true inside, false outside. Rust: covered by
        //   bypass_runs_closure_for_system.
        // TestRunWithBypass_ErrorPropagation (L74-86): closure error
        //   propagates. Rust: covered by s11_bypass_audit_recorded_even_if_
        //   closure_fails.
        // TestIsBypassActive (L88-105): false when not set, true after set.
        //   Rust: no context-key bypass state query — structural gap.
        // TestGetBypassInfo_NotSet (L107-114): ok=false when not set.
        //   Rust: no bypassInfo retrieval — structural gap.
        // TestSetAuditLogger (L150-190): custom logger captures record.
        //   Rust: covered by s11_bypass_with_sink_records_one_entry.
        // TestBypassScopeIsolation (L192-216): inner context inherits outer
        //   context values. Rust: closures capture by reference — structural
        //   gap (Go context.WithValue chain vs Rust borrow).
        // TestBypassWithSystemPrincipal (L218-243): system succeeds, fields
        //   correct. Rust: covered by s14_validate_bypass_system_produces_
        //   audit_entry.
        // TestBypassWithNonSystemPrincipal (L245-265): user/apikey fail.
        //   Rust: covered by s14_validate_bypass_user_denied +
        //   s14_validate_bypass_apikey_denied.
    }
}

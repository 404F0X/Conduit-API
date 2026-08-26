use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::principal::Principal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RequestSource {
    #[default]
    Unknown,
    AdminRest,
    OpenAi,
    Anthropic,
    Gemini,
    Internal,
    Test,
}

impl fmt::Display for RequestSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown",
            Self::AdminRest => "admin_rest",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Internal => "internal",
            Self::Test => "test",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("request context field `{field}` already set to a different value")]
pub struct ContextConflictError {
    pub field: &'static str,
}

// ---- Typed sub-contexts (S12/S13/S16/S17) ---------------------------------
//
// These mirror the Go `*ent.*` entities that `conduit/internal/contexts` attaches
// to the request context (User, APIKey, Thread, Trace). They carry only
// identifying, non-secret fields — the Go container stores the full entity, but
// the secret-bearing fields (e.g. `APIKey.Key`, `User.Password`) are never
// propagated through the typed context surface here; downstream services read
// them from the DB layer. We keep these as plain id-bearing structs so the
// `RequestContext` stays serializable/redactable.

/// User identity attached to a request.
///
/// Go source: `contexts.WithUser(ctx, *ent.User)` where `ent.User` fields are
/// `id, email, status, prefer_language, first_name, last_name, avatar,
/// is_owner, scopes`. Only non-secret identifying fields are mirrored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserContext {
    pub user_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Whether the user is the project/system owner.
    #[serde(default)]
    pub is_owner: bool,
}

/// API key identity attached to a request.
///
/// Go source: `contexts.WithAPIKey(ctx, *ent.APIKey)` where `ent.APIKey`
/// fields are `id, user_id, project_id, key, name, type, status, scopes,
/// profiles`. The secret `key` field is intentionally NOT mirrored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyContext {
    pub api_key_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    pub project_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Trace identity attached to a request.
///
/// Go source: `contexts.WithTrace(ctx, *ent.Trace)` where `ent.Trace` fields
/// are `id, project_id, trace_id, thread_id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceContext {
    pub trace_db_id: i64,
    pub project_id: i64,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
}

/// Thread identity attached to a request.
///
/// Go source: `contexts.WithThread(ctx, *ent.Thread)` where `ent.Thread`
/// fields are `id, project_id, thread_id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadContext {
    pub thread_db_id: i64,
    pub project_id: i64,
    pub thread_id: String,
}

#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RequestContext {
    pub principal: Option<Principal>,
    pub project_id: Option<String>,
    pub source: Option<RequestSource>,
    pub session_id: Option<String>,
    pub client_ip: Option<String>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    // S12/S13/S16/S17 — typed sub-contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadContext>,
}

impl RequestContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_system_bypass(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let mut ctx = Self::new();
        ctx.principal = Some(Principal::system().with_scope(format!("bypass:{reason}")));
        ctx.source = Some(RequestSource::Internal);
        ctx
    }

    pub fn run_with_bypass<T>(
        reason: impl Into<String>,
        f: impl FnOnce(&RequestContext) -> T,
    ) -> T {
        let ctx = Self::with_system_bypass(reason);
        f(&ctx)
    }

    pub fn set_principal(&mut self, principal: Principal) -> Result<(), ContextConflictError> {
        set_once("principal", &mut self.principal, principal)
    }

    pub fn set_project_id(
        &mut self,
        project_id: impl Into<String>,
    ) -> Result<(), ContextConflictError> {
        set_once("project_id", &mut self.project_id, project_id.into())
    }

    pub fn set_source(&mut self, source: RequestSource) -> Result<(), ContextConflictError> {
        set_once("source", &mut self.source, source)
    }

    pub fn set_session_id(
        &mut self,
        session_id: impl Into<String>,
    ) -> Result<(), ContextConflictError> {
        set_once("session_id", &mut self.session_id, session_id.into())
    }

    pub fn set_client_ip(
        &mut self,
        client_ip: impl Into<String>,
    ) -> Result<(), ContextConflictError> {
        set_once("client_ip", &mut self.client_ip, client_ip.into())
    }

    pub fn set_request_id(
        &mut self,
        request_id: impl Into<String>,
    ) -> Result<(), ContextConflictError> {
        set_once("request_id", &mut self.request_id, request_id.into())
    }

    pub fn set_trace_id(
        &mut self,
        trace_id: impl Into<String>,
    ) -> Result<(), ContextConflictError> {
        set_once("trace_id", &mut self.trace_id, trace_id.into())
    }

    /// Attaches a typed user context (S12). Idempotent on equal; conflicts on
    /// a previously-set different value, mirroring `set_once` semantics.
    pub fn set_user(&mut self, user: UserContext) -> Result<(), ContextConflictError> {
        set_once("user", &mut self.user, user)
    }

    /// Attaches a typed API-key context (S13).
    pub fn set_api_key(&mut self, api_key: ApiKeyContext) -> Result<(), ContextConflictError> {
        set_once("api_key", &mut self.api_key, api_key)
    }

    /// Attaches a typed trace context (S16).
    pub fn set_trace(&mut self, trace: TraceContext) -> Result<(), ContextConflictError> {
        set_once("trace", &mut self.trace, trace)
    }

    /// Attaches a typed thread context (S17).
    pub fn set_thread(&mut self, thread: ThreadContext) -> Result<(), ContextConflictError> {
        set_once("thread", &mut self.thread, thread)
    }

    pub fn safe_summary(&self) -> String {
        format!(
            "principal={}, project_id={}, source={}, session_id={}, user={}, api_key={}, trace={}, thread={}",
            self.principal
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "anonymous".to_string()),
            self.project_id.as_deref().unwrap_or("none"),
            self.source
                .map(|source| source.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            self.session_id.as_deref().unwrap_or("none"),
            self.user
                .as_ref()
                .map(|u| u.user_id.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.api_key
                .as_ref()
                .map(|k| k.api_key_id.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.trace
                .as_ref()
                .map(|t| t.trace_id.clone())
                .unwrap_or_else(|| "none".to_string()),
            self.thread
                .as_ref()
                .map(|t| t.thread_id.clone())
                .unwrap_or_else(|| "none".to_string()),
        )
    }
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestContext")
            .field("principal", &self.principal)
            .field("project_id", &self.project_id)
            .field("source", &self.source)
            .field("session_id", &self.session_id)
            .field("client_ip", &self.client_ip)
            .field("request_id", &self.request_id)
            .field("trace_id", &self.trace_id)
            .field("user", &self.user)
            .field("api_key", &self.api_key)
            .field("trace", &self.trace)
            .field("thread", &self.thread)
            .finish()
    }
}

impl fmt::Display for RequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.safe_summary())
    }
}

fn set_once<T>(
    field: &'static str,
    slot: &mut Option<T>,
    value: T,
) -> Result<(), ContextConflictError>
where
    T: PartialEq,
{
    match slot {
        Some(existing) if *existing == value => Ok(()),
        Some(_) => Err(ContextConflictError { field }),
        None => {
            *slot = Some(value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::Principal;

    #[test]
    fn set_once_principal_sets_value() -> Result<(), ContextConflictError> {
        let mut ctx = RequestContext::new();
        ctx.set_principal(Principal::user("user-1"))?;

        assert_eq!(
            ctx.principal.as_ref().map(ToString::to_string),
            Some("user:user-1".to_string())
        );
        Ok(())
    }

    #[test]
    fn conflict_returns_error() -> Result<(), ContextConflictError> {
        let mut ctx = RequestContext::new();
        ctx.set_project_id("project-1")?;

        let res = ctx.set_project_id("project-2");

        assert!(matches!(res, Err(ref e) if e.field == "project_id"));
        Ok(())
    }

    #[test]
    fn repeated_same_value_is_idempotent() -> Result<(), ContextConflictError> {
        let mut ctx = RequestContext::new();

        ctx.set_source(RequestSource::OpenAi)?;
        ctx.set_source(RequestSource::OpenAi)?;
        ctx.set_session_id("session-1")?;
        ctx.set_session_id("session-1")?;

        assert_eq!(ctx.source, Some(RequestSource::OpenAi));
        assert_eq!(ctx.session_id.as_deref(), Some("session-1"));
        Ok(())
    }

    #[test]
    fn string_helpers_are_safe_and_stable() -> Result<(), ContextConflictError> {
        let mut ctx = RequestContext::new();
        ctx.set_principal(Principal::api_key("key-id", "project-1"))?;
        ctx.set_project_id("project-1")?;
        ctx.set_source(RequestSource::OpenAi)?;
        ctx.set_session_id("session-1")?;

        let rendered = ctx.to_string();
        let debug = format!("{ctx:?}");

        assert!(rendered.contains("principal=apikey:key-id"));
        assert!(rendered.contains("project_id=project-1"));
        assert!(rendered.contains("source=openai"));
        assert!(!rendered.contains("secret"));
        assert!(!debug.contains("secret"));
        Ok(())
    }

    #[test]
    fn bypass_context_is_system_principal() {
        let result = RequestContext::run_with_bypass("migration", |ctx| {
            ctx.principal.as_ref().map(ToString::to_string)
        });

        assert_eq!(result, Some("system".to_string()));
    }
}

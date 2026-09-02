pub mod api_key_auth;
pub mod jwt_auth;
pub mod metrics;
pub mod runtime;

use std::collections::HashSet;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{HeaderMap, Method, Request, header};
use conduit_core::ConduitError;
use serde_json::Value;

pub const DEFAULT_BODY_COLLECT_LIMIT_BYTES: usize = 1024 * 1024;
const REQUEST_BODY_TOO_LARGE_STATUS: u16 = 413;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordedMiddleware {
    AccessLog,
    RequestContext,
    LoggingTracing,
    Metrics,
    IpBlocklist,
    Cors,
    Timeout,
    Auth,
    Source,
    Thread,
    Trace,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestSource {
    SourceAPI,
    SourcePlayground,
}

pub fn source_for_route(is_playground: bool) -> RequestSource {
    if is_playground {
        RequestSource::SourcePlayground
    } else {
        RequestSource::SourceAPI
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RequestPrincipal {
    subject: String,
}

impl RequestPrincipal {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
        }
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }
}

impl fmt::Debug for RequestPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestPrincipal")
            .field("subject", &self.subject)
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct RawRequestHeaders(HeaderMap);

impl RawRequestHeaders {
    pub fn new(headers: HeaderMap) -> Self {
        Self(headers)
    }

    pub fn as_header_map(&self) -> &HeaderMap {
        &self.0
    }
}

impl fmt::Debug for RawRequestHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.keys().map(|name| name.as_str()).collect();

        f.debug_struct("RawRequestHeaders")
            .field("len", &self.0.len())
            .field("names", &names)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequestContext {
    principal: Option<RequestPrincipal>,
    source: RequestSource,
    client_ip: Option<IpAddr>,
    raw_headers: RawRequestHeaders,
}

impl HttpRequestContext {
    pub fn new(
        source: RequestSource,
        principal: Option<RequestPrincipal>,
        client_ip: Option<IpAddr>,
        raw_headers: RawRequestHeaders,
    ) -> Self {
        Self {
            principal,
            source,
            client_ip,
            raw_headers,
        }
    }

    pub fn principal(&self) -> Option<&RequestPrincipal> {
        self.principal.as_ref()
    }

    pub fn source(&self) -> RequestSource {
        self.source
    }

    pub fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    pub fn raw_headers(&self) -> &RawRequestHeaders {
        &self.raw_headers
    }
}

impl fmt::Debug for HttpRequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequestContext")
            .field("principal", &self.principal)
            .field("source", &self.source)
            .field("client_ip", &self.client_ip)
            .field("raw_headers", &self.raw_headers)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RequestContextExtension(HttpRequestContext);

impl RequestContextExtension {
    pub fn new(context: HttpRequestContext) -> Self {
        Self(context)
    }

    pub fn context(&self) -> &HttpRequestContext {
        &self.0
    }

    pub fn into_context(self) -> HttpRequestContext {
        self.0
    }
}

impl fmt::Debug for RequestContextExtension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RequestContextExtension")
            .field(&self.0)
            .finish()
    }
}

// ===========================================================================
// RUST-P11-001 S17 — RequestContext 注入中间件 (Zeno-the-3rd)
//
// Go 在 `internal/server/middleware/auth.go` 各 `With*Auth` 完成鉴权后,通过
// `contexts.WithAPIKey` / `contexts.WithUser` / `contexts.WithProjectID` /
// `shared.WithSessionScope` / `authz.WithPrincipal` 把鉴权结果注入到
// `c.Request.Context()`,handler 通过 `contexts.FromContext(ctx)` 取出,
// **不再自查 API key/project**。`project.go` 同样把 `X-Project-ID` header
// 解析后写入 ctx。
//
// Rust 端等价物:axum 的 `http::Extensions` 按具体类型索引,因此我们用一个
// 强类型 `AuthRequestContextExtension` 包裹 `conduit_auth::RequestContext`,
// 鉴权中间件负责构造并 `extensions_mut().insert`,handler 通过
// `axum::Extension<AuthRequestContextExtension>` 取出。
//
// 设计选择(最小改动):
// - 不改现有 `RequestContextExtension`(它只承载 `HttpRequestContext`,用于
//   access-log / raw-headers 等,P2-002 已固化)。
// - 新增独立的 `AuthRequestContextExtension` 承载 `conduit_auth::RequestContext`
//   (含 principal / project_id / source / request_id / typed sub-contexts),
//   handler 从该 extension 取鉴权结果——不直接调用 `extract_api_key`。
// - 提供纯逻辑 helper(`build_context_from_api_key_auth` / `build_context_from_jwt`
//   / `apply_project_id_header` / `apply_jwt_claims`),由 router.rs 的 tower
//   Layer 在 middleware 链中调用;本模块保持可单测。
// ===========================================================================

/// 鉴权后注入到请求扩展的强类型 wrapper,handler 通过
/// `axum::Extension<AuthRequestContextExtension>` 取出 `RequestContext`。
///
/// Go 等价:`contexts.FromContext(ctx)` 返回
/// `*authz.Principal` + project_id + user / api_key entity 等。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequestContextExtension(conduit_auth::RequestContext);

impl AuthRequestContextExtension {
    pub fn new(context: conduit_auth::RequestContext) -> Self {
        Self(context)
    }

    pub fn context(&self) -> &conduit_auth::RequestContext {
        &self.0
    }

    pub fn into_context(self) -> conduit_auth::RequestContext {
        self.0
    }
}

/// P-22: whether the JWT-authenticated caller may read request rows
/// (prompt/response content + previews).
///
/// Mirrors Go's `Request` ent policy branches: global `read_requests` grants
/// cross-project access, while a project membership/role grant is accepted only
/// for the project selected in the authenticated request context.
pub fn caller_can_read_requests(auth: Option<&AuthRequestContextExtension>) -> bool {
    let Some(context) = auth.map(AuthRequestContextExtension::context) else {
        return false;
    };
    conduit_auth::rbac::has_scope(context, conduit_auth::scopes::slug::READ_REQUESTS).is_allowed()
        || conduit_auth::rbac::has_project_scope(context, conduit_auth::scopes::slug::READ_REQUESTS)
            .is_allowed()
}

/// 把已构造的 `RequestContext` 注入到请求扩展中。
///
/// 返回先前插入的同类型值(如有),与 `axum::Extensions::insert` 语义一致。
/// 对应 Go `c.Request = c.Request.WithContext(ctx)`。
pub fn insert_auth_request_context<B>(
    request: &mut Request<B>,
    context: AuthRequestContextExtension,
) -> Option<AuthRequestContextExtension> {
    request.extensions_mut().insert(context)
}

/// 从请求扩展取出鉴权上下文(handler 入口用)。
///
/// Go 等价:`contexts.FromContext(ctx)`。返回 `None` 意味着鉴权中间件未运行,
/// handler 应据此返回 401/500 而非自行调用 `extract_api_key`。
pub fn extract_auth_request_context<B>(
    request: &Request<B>,
) -> Option<&AuthRequestContextExtension> {
    request.extensions().get::<AuthRequestContextExtension>()
}

/// 构造 API key 鉴权后的 `RequestContext`。
///
/// 镜像 Go `WithAPIKeyConfig` L54-68:
/// - `contexts.WithAPIKey(ctx, apiKey)` + `WithProjectID` (若 `apiKey.Edges.Project`
///   非空) + `withSessionScopeForAPIKey` + `withAPIKeyPrincipal`。
/// - principal 的 `session_scope` 与 `safe_subject` 已由 `conduit_auth::Principal`
///   构造器内置(与 Go `withSessionScopeForAPIKey` 同格式)。
///
/// 入参只接收"鉴权后已确定的事实"(principal / project_id / source),**绝不**
/// 接收原始 API key 字符串——handler 不能再用原始 key 自查。
pub fn build_context_from_api_key_auth(
    principal: conduit_auth::Principal,
    project_id: Option<String>,
    source: conduit_auth::RequestSource,
    request_id: Option<String>,
    client_ip: Option<String>,
) -> conduit_auth::RequestContext {
    let mut ctx = conduit_auth::RequestContext::new();
    // set_once 在重复设置同值时幂等;此处顺序构建,任一字段首次设置必成功。
    let _ = ctx.set_principal(principal);
    if let Some(pid) = project_id {
        let _ = ctx.set_project_id(pid);
    }
    let _ = ctx.set_source(source);
    if let Some(rid) = request_id {
        let _ = ctx.set_request_id(rid);
    }
    if let Some(ip) = client_ip {
        let _ = ctx.set_client_ip(ip);
    }
    ctx
}

/// 构造 JWT 鉴权后的 `RequestContext`。
///
/// 镜像 Go `WithJWTAuth` L96-106:
/// - `contexts.WithUser(ctx, user)`
/// - `shared.WithSessionScope(ctx, "user:"+strconv.Itoa(user.ID))`
/// - `withUserPrincipal(ctx, user)` -> `authz.Principal{Type: User, UserID: &user.ID}`
///
/// 入参 `user_id` 对应 Go `user.ID`(int),`conduit_auth::Principal::user` 会
/// 自动构造 `user:<id>` 的 session_scope。
pub fn build_context_from_jwt(
    user_id: i64,
    source: conduit_auth::RequestSource,
    request_id: Option<String>,
    client_ip: Option<String>,
) -> conduit_auth::RequestContext {
    let principal = conduit_auth::Principal::user(user_id.to_string());
    let user_ctx = conduit_auth::request_context::UserContext {
        user_id,
        ..Default::default()
    };

    let mut ctx = conduit_auth::RequestContext::new();
    let _ = ctx.set_principal(principal);
    let _ = ctx.set_source(source);
    if let Some(rid) = request_id {
        let _ = ctx.set_request_id(rid);
    }
    if let Some(ip) = client_ip {
        let _ = ctx.set_client_ip(ip);
    }
    let _ = ctx.set_user(user_ctx);
    ctx
}

/// The identity facts Go's `WithJWTAuth` obtains by loading the full
/// `*ent.User` after verifying the token (`biz/auth.go:190-201`
/// `AuthenticateJWTToken` → `contexts.WithUser`).
///
/// Go's *principal* carries only the user id (`withUserPrincipal`,
/// `middleware/auth.go:227-230`), but every scope rule reads the loaded user:
/// `scopes.UserHasScope` → `contexts.GetUser(ctx)` → `userHasSystemScope`
/// (`rule_user_scope.go:66-77`), and the owner rule reads `user.IsOwner`
/// (`rule_owner.go`). Rust folds those same facts onto the principal, because
/// `conduit_auth::rbac` reads `principal.scopes` / `principal.is_owner`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JwtUserIdentity {
    /// Go `user.IsOwner` — the owner rule short-circuits every scope check.
    pub is_owner: bool,
    /// Go `user.Scopes` (the role-expanded system scope slugs).
    pub scope_slugs: Vec<String>,
}

/// Outcome of resolving a JWT user's identity facts (three-state contract).
///
/// Go `AuthenticateJWTToken` (`biz/auth.go:192-201`) makes the user load part
/// of *authentication*: a failed lookup (`failed to get user`), a missing row,
/// or `Status != activated` (`user not activated`) all wrap `ErrInvalidJWT`,
/// which `WithJWTAuth` maps to **401 "Invalid token"**. The Rust middleware
/// mirrors that: [`UserUnavailable`](Self::UserUnavailable) is a hard 401, not
/// a "skip enrichment" no-op — otherwise a deactivated/deleted user's still
/// unexpired JWT would keep working until expiry.
///
/// The third state — "no resolver wired at all" — is encoded by the *absence*
/// of a resolver in `AppServices` (`user_principal_service() == None`), which
/// preserves back-compat for hosts/tests that wire only the JWT secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtIdentityResolution {
    /// User exists and is activated: fold these facts onto the principal.
    Found(JwtUserIdentity),
    /// User missing / deactivated / lookup failed — Go returns `ErrInvalidJWT`
    /// here, so the middleware must answer 401 (same public message as any
    /// other invalid token; the account state is not leaked).
    UserUnavailable,
}

/// Resolve the identity facts for a JWT-authenticated user id.
///
/// This is the user-load half of Go `AuthenticateJWTToken`: signature
/// verification alone is *not* full authentication — the user row must exist
/// and be activated (`biz/auth.go:192-201`). Hosts wire a DB-backed
/// implementation (`DbJwtIdentityResolver` in the binary); when no resolver is
/// wired the middleware skips this step entirely (legacy secret-only wiring).
#[async_trait::async_trait]
pub trait JwtIdentityResolver: Send + Sync {
    async fn resolve(&self, user_id: i64) -> JwtIdentityResolution;
}

/// Fold the loaded user's owner flag + system scopes onto the JWT principal.
///
/// Without this the principal built by [`build_context_from_jwt`] carries an
/// empty scope set and `is_owner = false`, so `conduit_auth::rbac` denies every
/// scope check. Enriching here is what makes a real per-user principal usable
/// end to end.
pub fn enrich_jwt_context(
    mut ctx: conduit_auth::RequestContext,
    identity: &JwtUserIdentity,
) -> conduit_auth::RequestContext {
    let Some(principal) = ctx.principal.take() else {
        return ctx;
    };
    let mut principal = principal.with_owner(identity.is_owner);
    for scope in &identity.scope_slugs {
        principal = principal.with_scope(scope.clone());
    }
    let _ = ctx.set_principal(principal);
    ctx
}

/// 把 `X-Project-ID` header 解析后的 project_id 写入已有 `RequestContext`。
///
/// 镜像 Go `project.go` L14-33:header 缺失 -> 放行(不修改 ctx);非法 GUID
/// 或 type!=Project -> 400 `Invalid project ID`;成功 -> `WithProjectID`。
///
/// **关键**:handler 不应自行解析该 header;由本中间件统一注入,handler 只读 ctx。
pub fn apply_project_id_header(
    ctx: &mut conduit_auth::RequestContext,
    headers: &HeaderMap,
) -> Result<(), ConduitError> {
    // 复用同模块 `extract_project_id` 的 header/query/body 优先级;此处 Go 行为
    // 是 header-only,query/body 兜底由 `extract_project_id` 提供(向后兼容),
    // 不破坏 Go 契约——header 存在时优先级最高。
    let extracted = extract_project_id(headers, None, None)?;
    if let Some(found) = extracted {
        let _ = ctx.set_project_id(found.id.to_string());
    }
    Ok(())
}

/// 校验 JWT 并把结果映射为 `RequestContext`。
///
/// 镜像 Go `WithJWTAuth` L85-94:`auth.AuthenticateJWTToken` 成功 -> user;
/// `ErrInvalidJWT` -> 401 `Invalid token`;其它错误 -> 500
/// `Failed to validate token`。本函数把已签名 token 的解码委托给
/// `conduit_auth::jwt::decode_hs256`,失败映射到 `JwtAuthError`(供
/// `jwt_auth_outcome` 决定对外响应)。
pub fn verify_jwt_and_build_context(
    token: &str,
    secret: &[u8],
    source: conduit_auth::RequestSource,
    request_id: Option<String>,
    client_ip: Option<String>,
) -> Result<conduit_auth::RequestContext, JwtAuthError> {
    match conduit_auth::jwt::decode_hs256(token, secret) {
        Ok(claims) => Ok(build_context_from_jwt(
            claims.user_id,
            source,
            request_id,
            client_ip,
        )),
        Err(_) => {
            // 区分"token 无效"(签名错/过期/格式错 -> Invalid)与"服务端故障"。
            // `jsonwebtoken` 错误统一映射到 Invalid —— Go `biz.ErrInvalidJWT`
            // 同样涵盖所有 jwt 校验失败;真正的 5xx(Go 中只有 DB/cache 故障)
            // 在 Rust 这条纯 jwt 路径不会出现。
            Err(JwtAuthError::Invalid)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceContext {
    trace_id: Option<String>,
    request_body_snapshot: Option<String>,
}

impl TraceContext {
    pub fn new(trace_id: Option<String>, request_body_snapshot: Option<String>) -> Self {
        Self {
            trace_id,
            request_body_snapshot,
        }
    }

    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    pub fn request_body_snapshot(&self) -> Option<&str> {
        self.request_body_snapshot.as_deref()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadContext {
    thread_id: Option<String>,
}

impl ThreadContext {
    pub fn new(thread_id: Option<String>) -> Self {
        Self { thread_id }
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceThreadContext {
    trace: TraceContext,
    thread: ThreadContext,
}

impl TraceThreadContext {
    pub fn new(trace: TraceContext, thread: ThreadContext) -> Self {
        Self { trace, thread }
    }

    pub fn from_inputs(
        headers: &HeaderMap,
        query: Option<&str>,
        body_metadata: Option<&Value>,
        body_snapshot: Option<String>,
    ) -> Self {
        let trace_id = resolve_context_value(
            headers,
            query,
            body_metadata,
            "x-trace-id",
            &["trace_id", "traceId"],
        );
        let thread_id = resolve_context_value(
            headers,
            query,
            body_metadata,
            "x-thread-id",
            &["thread_id", "threadId"],
        );

        Self::new(
            TraceContext::new(trace_id, body_snapshot),
            ThreadContext::new(thread_id),
        )
    }

    pub fn trace(&self) -> &TraceContext {
        &self.trace
    }

    pub fn thread(&self) -> &ThreadContext {
        &self.thread
    }

    pub fn trace_id(&self) -> Option<&str> {
        self.trace.trace_id()
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread.thread_id()
    }

    pub fn request_body_snapshot(&self) -> Option<&str> {
        self.trace.request_body_snapshot()
    }
}

pub fn request_context_for_route(
    is_playground: bool,
    principal: Option<RequestPrincipal>,
    client_ip: Option<IpAddr>,
    raw_headers: HeaderMap,
) -> RequestContextExtension {
    let source = source_for_route(is_playground);
    let context = HttpRequestContext::new(
        source,
        principal,
        client_ip,
        RawRequestHeaders::new(raw_headers),
    );

    RequestContextExtension::new(context)
}

pub fn insert_request_context<B>(
    request: &mut Request<B>,
    context: RequestContextExtension,
) -> Option<RequestContextExtension> {
    // http::Extensions is keyed by concrete type, so downstream code avoids
    // stringly-typed context keys while middleware order is still being built.
    request.extensions_mut().insert(context)
}

pub fn extract_request_context<B>(request: &Request<B>) -> Option<&RequestContextExtension> {
    request.extensions().get::<RequestContextExtension>()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsConfig {
    allow_any_origin: bool,
    allowed_origins: Vec<String>,
    allowed_methods: Vec<Method>,
}

impl CorsConfig {
    pub fn permissive() -> Self {
        Self {
            allow_any_origin: true,
            allowed_origins: Vec::new(),
            allowed_methods: Vec::new(),
        }
    }

    pub fn new(allowed_origins: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allow_any_origin: false,
            allowed_origins: allowed_origins.into_iter().map(Into::into).collect(),
            allowed_methods: Vec::new(),
        }
    }

    pub fn with_allowed_methods(mut self, methods: impl IntoIterator<Item = Method>) -> Self {
        self.allowed_methods = methods.into_iter().collect();
        self
    }

    pub fn decide(&self, method: &Method, headers: &HeaderMap) -> CorsDecision {
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        let requested_method = headers
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<Method>().ok());
        let is_preflight =
            method == Method::OPTIONS && origin.is_some() && requested_method.is_some();
        let origin_allowed = origin.is_none_or(|origin| self.origin_allowed(origin));
        let method_allowed = requested_method
            .as_ref()
            .is_none_or(|method| self.method_allowed(method));

        CorsDecision {
            kind: if is_preflight {
                CorsRequestKind::Preflight
            } else {
                CorsRequestKind::Actual
            },
            origin_allowed,
            method_allowed,
            auth_required: !is_preflight,
        }
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.allow_any_origin || self.allowed_origins.iter().any(|allowed| allowed == origin)
    }

    fn method_allowed(&self, method: &Method) -> bool {
        self.allowed_methods.is_empty()
            || self.allowed_methods.iter().any(|allowed| allowed == method)
    }
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self::permissive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorsRequestKind {
    Preflight,
    Actual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorsDecision {
    kind: CorsRequestKind,
    origin_allowed: bool,
    method_allowed: bool,
    auth_required: bool,
}

impl CorsDecision {
    pub fn kind(&self) -> CorsRequestKind {
        self.kind
    }

    pub fn origin_allowed(&self) -> bool {
        self.origin_allowed
    }

    pub fn method_allowed(&self) -> bool {
        self.method_allowed
    }

    pub fn auth_required(&self) -> bool {
        self.auth_required
    }

    pub fn allowed(&self) -> bool {
        self.origin_allowed && self.method_allowed
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpBlocklist {
    blocked: HashSet<IpAddr>,
}

impl IpBlocklist {
    pub fn new(blocked: impl IntoIterator<Item = IpAddr>) -> Self {
        Self {
            blocked: blocked.into_iter().collect(),
        }
    }

    pub fn decide(&self, client_ip: Option<IpAddr>) -> IpDecision {
        match client_ip {
            Some(ip) if self.blocked.contains(&ip) => IpDecision::Blocked(ip),
            _ => IpDecision::Allowed,
        }
    }

    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        self.blocked.contains(&ip)
    }
}

/// Match client IP candidates against exact addresses and CIDR prefixes.
/// Invalid candidates and invalid configured entries are ignored, matching
/// the original Go middleware's fail-open parsing behavior.
pub fn is_blocked_ip(client_ips: &[String], blocked_ips: &[String]) -> bool {
    client_ips.iter().any(|candidate| {
        let candidate = candidate.trim();
        let Ok(client) = candidate.parse::<IpAddr>() else {
            tracing::warn!(client_ip = candidate, "failed to parse client IP");
            return false;
        };

        blocked_ips
            .iter()
            .any(|configured| blocked_entry_matches(client, configured))
    })
}

fn blocked_entry_matches(client: IpAddr, configured: &str) -> bool {
    let configured = configured.trim();
    if configured.is_empty() {
        return false;
    }

    let Some((network, prefix_len)) = configured.split_once('/') else {
        return match configured.parse::<IpAddr>() {
            Ok(blocked) => canonical_ip(blocked) == canonical_ip(client),
            Err(error) => {
                tracing::warn!(blocked_ip = configured, %error, "failed to parse blocked IP");
                false
            }
        };
    };

    let Ok(network) = network.trim().parse::<IpAddr>() else {
        tracing::warn!(blocked_ip = configured, "failed to parse blocked IP prefix");
        return false;
    };
    let Ok(prefix_len) = prefix_len.trim().parse::<u8>() else {
        tracing::warn!(
            blocked_ip = configured,
            "failed to parse blocked IP prefix length"
        );
        return false;
    };

    match (client, network) {
        (IpAddr::V4(client), IpAddr::V4(network)) if prefix_len <= 32 => {
            let mask = if prefix_len == 0 {
                0
            } else {
                u32::MAX << (32 - prefix_len)
            };
            (u32::from(client) & mask) == (u32::from(network) & mask)
        }
        (IpAddr::V6(client), IpAddr::V4(network)) if prefix_len <= 32 => {
            let Some(client) = client.to_ipv4_mapped() else {
                return false;
            };
            let mask = if prefix_len == 0 {
                0
            } else {
                u32::MAX << (32 - prefix_len)
            };
            (u32::from(client) & mask) == (u32::from(network) & mask)
        }
        (IpAddr::V6(client), IpAddr::V6(network)) if prefix_len <= 128 => {
            let mask = if prefix_len == 0 {
                0
            } else {
                u128::MAX << (128 - prefix_len)
            };
            (u128::from(client) & mask) == (u128::from(network) & mask)
        }
        _ => {
            tracing::warn!(blocked_ip = configured, "failed to parse blocked IP prefix");
            false
        }
    }
}

/// Treat an IPv4-mapped IPv6 address as its canonical IPv4 identity. Socket
/// peers and forwarding proxies may represent the same client either way, so
/// exact blocklist and trusted-proxy checks must not distinguish the forms.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        IpAddr::V4(ip) => IpAddr::V4(ip),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpDecision {
    Allowed,
    Blocked(IpAddr),
}

impl IpDecision {
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestMiddlewareDecision {
    BlockedIp(IpAddr),
    Cors(CorsDecision),
}

pub fn decide_request_middleware(
    blocklist: &IpBlocklist,
    cors: &CorsConfig,
    client_ip: Option<IpAddr>,
    method: &Method,
    headers: &HeaderMap,
) -> RequestMiddlewareDecision {
    match blocklist.decide(client_ip) {
        IpDecision::Blocked(ip) => RequestMiddlewareDecision::BlockedIp(ip),
        IpDecision::Allowed => RequestMiddlewareDecision::Cors(cors.decide(method, headers)),
    }
}

pub fn middleware_order() -> &'static [RecordedMiddleware] {
    &[
        RecordedMiddleware::AccessLog,
        RecordedMiddleware::RequestContext,
        RecordedMiddleware::LoggingTracing,
        RecordedMiddleware::Metrics,
        RecordedMiddleware::IpBlocklist,
        RecordedMiddleware::Cors,
        RecordedMiddleware::Timeout,
        RecordedMiddleware::Auth,
        RecordedMiddleware::Source,
        RecordedMiddleware::Thread,
        RecordedMiddleware::Trace,
        RecordedMiddleware::Project,
    ]
}

#[derive(Clone, Debug, Default)]
pub struct MiddlewareOrderRecorder {
    seen: Vec<RecordedMiddleware>,
}

impl MiddlewareOrderRecorder {
    pub fn record(&mut self, middleware: RecordedMiddleware) {
        self.seen.push(middleware);
    }

    pub fn record_order(&mut self, order: &[RecordedMiddleware]) {
        self.seen.extend_from_slice(order);
    }

    pub fn seen(&self) -> Vec<RecordedMiddleware> {
        self.seen.clone()
    }
}

pub fn record_middleware_order(recorder: &mut MiddlewareOrderRecorder) {
    recorder.record_order(middleware_order());
}

#[derive(Debug, PartialEq, Eq)]
pub enum BodyRewindError {
    BodyReadFailed(String),
    BodyTooLarge { limit: usize },
}

impl fmt::Display for BodyRewindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyReadFailed(message) => write!(f, "body read failed: {message}"),
            Self::BodyTooLarge { limit } => write!(f, "body exceeded limit of {limit} bytes"),
        }
    }
}

impl std::error::Error for BodyRewindError {}

impl From<BodyRewindError> for ConduitError {
    fn from(err: BodyRewindError) -> Self {
        match err {
            BodyRewindError::BodyReadFailed(message) => {
                ConduitError::invalid_request(format!("request body could not be read: {message}"))
            }
            BodyRewindError::BodyTooLarge { limit } => ConduitError::invalid_request(format!(
                "request body exceeds limit of {limit} bytes"
            ))
            .with_http_status(REQUEST_BODY_TOO_LARGE_STATUS),
        }
    }
}

pub async fn body_collect_limit(
    request: Request<Body>,
    limit: usize,
) -> Result<(Request<Body>, Bytes), BodyRewindError> {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, limit)
        .await
        .map_err(|err| map_body_error(err.to_string(), limit))?;
    let request = Request::from_parts(parts, Body::from(bytes.clone()));

    Ok((request, bytes))
}

pub async fn rewind_body(
    request: Request<Body>,
) -> Result<(Request<Body>, Bytes), BodyRewindError> {
    body_collect_limit(request, DEFAULT_BODY_COLLECT_LIMIT_BYTES).await
}

pub fn body_collect_limit_bytes() -> usize {
    DEFAULT_BODY_COLLECT_LIMIT_BYTES
}

// ---- S12 Body extraction 后必须恢复 body -------------------------------------

/// `extract-then-restore` 的产物：提取出的 trace id 与恢复后的请求（body 仍可被下游读）。
///
/// 对齐 Go `trace.go` 的关键不变量：每次 `io.ReadAll(c.Request.Body)` 之后都紧跟着
/// `c.Request.Body = io.NopCloser(bytes.NewReader(body))`，保证下游 handler 仍能读到
/// 完整 body。本结构体把该契约固化——调用方拿到的 `restored_request` 其 body 与
/// `body_bytes` 内容完全一致，可再次被 `to_bytes` 读取。
#[derive(Debug)]
pub struct ExtractedTraceWithRestoredBody {
    /// 从 body / header 解析出的 trace id（`None` 表示未提取到）。
    pub trace_id: Option<String>,
    /// 读到的原始 body 字节，供 snapshot / persistence 只读使用。
    pub body_bytes: Bytes,
    /// 恢复后的请求——body 与 `body_bytes` 内容一致，可继续向下游传递。
    pub restored_request: Request<Body>,
}

/// 固化 S12 契约：**读 body 提取 trace id 后必须把 body 恢复**。
///
/// 流程对应 Go `WithTrace` -> `tryGetTraceIDFromBody` / `tryExtractTraceIDFromClaudeCodeRequest`：
///
/// 1. `to_bytes` 读出 body（受 `limit` 保护，超限报 `BodyTooLarge`，对应 413）。
/// 2. 在字节切片上调用 `extractor` 提取 trace id（**只读借用**，绝不消耗）。
/// 3. 用同一份字节重建 `Request<Body>`——这一步即 Go 的
///    `c.Request.Body = io.NopCloser(bytes.NewReader(body))`，把读游标“倒回”起点。
///
/// 返回的 `restored_request` 可被下游再次 `to_bytes` 读取（见
/// `extract_then_restore_body_still_readable_downstream` 测试，镜像 Go 的不变量）。
///
/// `extractor` 接收 body 的不可变借用，因此多次提取 / 跨字段提取都不会“耗尽”body
/// （对应 `extract_then_restore_repeated_extractions_do_not_drain_body` 测试）。
pub async fn extract_trace_id_then_restore<F>(
    request: Request<Body>,
    limit: usize,
    extractor: F,
) -> Result<ExtractedTraceWithRestoredBody, BodyRewindError>
where
    F: FnOnce(&[u8]) -> Option<String>,
{
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, limit)
        .await
        .map_err(|err| map_body_error(err.to_string(), limit))?;

    // extractor 只拿不可变借用——body 不会被消耗，对应 S12 “提取后仍可读”。
    let trace_id = extractor(&bytes);

    // Go: c.Request.Body = io.NopCloser(bytes.NewReader(body))——用同一份字节重建 body。
    let restored_request = Request::from_parts(parts, Body::from(bytes.clone()));

    Ok(ExtractedTraceWithRestoredBody {
        trace_id,
        body_bytes: bytes,
        restored_request,
    })
}

// ---------------------------------------------------------------------------
// 以下为 RUST-P2-002 各 S 项的纯逻辑实现（不依赖 DB/service）。
// 行为镜像 conduit/internal/server/middleware/*.go；tower Layer 组装在
// router.rs 中完成，本模块只提供可单测的决策/解析函数。
// ---------------------------------------------------------------------------

/// `X-Project-ID` header 名称（与前端兼容，全大写连字符）。
pub const PROJECT_ID_HEADER: &str = "x-project-id";

/// GraphQL OpenAPI 端点路径前缀（service_account 鉴权专用）。
pub const OPENAPI_GRAPHQL_PATH: &str = "/openapi/v1/graphql";

/// `gid://conduit/{Type}/{ID}` 前缀（与 Go objects.GUID 一致，禁止臆测）。
const GUID_PREFIX: &str = "gid://conduit/";

/// Go `ent.TypeProject` 的字符串值。
const PROJECT_ENTITY_TYPE: &str = "Project";

/// JWT 失败时的对外错误消息（不泄露 jwt 内部原因，参考 auth.go L88/L90）。
pub const JWT_INVALID_PUBLIC_MESSAGE: &str = "Invalid token";

/// JWT 校验失败但属于服务端故障时的对外消息。
pub const JWT_INTERNAL_PUBLIC_MESSAGE: &str = "Failed to validate token";

/// API key 失败时的对外错误消息（不区分 NotFound/Invalid，参考 auth.go L44）。
pub const API_KEY_INVALID_PUBLIC_MESSAGE: &str = "Invalid API key";

/// API key 故障时的对外错误消息（参考 auth.go L48）。
pub const API_KEY_INTERNAL_PUBLIC_MESSAGE: &str = "Failed to validate API key";

/// Go `tracing.Config` 的最小子集，镜像默认 header 名。
///
/// Go 默认值：trace=`Conduit-Trace-Id`、request=`Conduit-Request-Id`、thread=`Conduit-Thread-Id`。
/// 这里只保留 middleware 需要的字段；其余字段在 conduit-config 中。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracingHeaderConfig {
    pub trace_header: String,
    pub request_header: String,
    pub thread_header: String,
    pub extra_trace_headers: Vec<String>,
    pub extra_trace_body_fields: Vec<String>,
    pub claude_code_trace_enabled: bool,
    pub codex_trace_enabled: bool,
    pub open_code_trace_enabled: bool,
}

impl Default for TracingHeaderConfig {
    fn default() -> Self {
        Self {
            trace_header: "Conduit-Trace-Id".to_string(),
            request_header: "Conduit-Request-Id".to_string(),
            thread_header: "Conduit-Thread-Id".to_string(),
            extra_trace_headers: Vec::new(),
            extra_trace_body_fields: Vec::new(),
            claude_code_trace_enabled: false,
            codex_trace_enabled: false,
            open_code_trace_enabled: false,
        }
    }
}

impl TracingHeaderConfig {
    /// 返回与 `tracing.go` 一致的有效 trace header 名（空则回退默认）。
    pub fn effective_trace_header(&self) -> &str {
        if self.trace_header.is_empty() {
            "Conduit-Trace-Id"
        } else {
            &self.trace_header
        }
    }

    /// 返回与 `logging.go` 一致的有效 request header 名。
    pub fn effective_request_header(&self) -> &str {
        if self.request_header.is_empty() {
            "Conduit-Request-Id"
        } else {
            &self.request_header
        }
    }

    /// 返回与 `thread.go` 一致的有效 thread header 名。
    pub fn effective_thread_header(&self) -> &str {
        if self.thread_header.is_empty() {
            "Conduit-Thread-Id"
        } else {
            &self.thread_header
        }
    }
}

// ---- S04 AccessLog ---------------------------------------------------------

/// AccessLog 决策（是否记录、记录哪些字段）。
///
/// 镜像 `access_log.go`：只在 `status >= 400` 或存在错误消息时才记录，避免
/// 给健康请求打 ERROR 日志。这里只产出结构化决策，实际日志写入由调用方完成。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessLogRecord {
    pub should_log: bool,
    pub status: u16,
    pub method: String,
    pub path: String,
    pub client_ip: Option<String>,
    pub operation: Option<String>,
    pub errors: Vec<String>,
}

/// 依据 Go `access_log.go` 判定是否应记录访问日志。
///
/// * `status >= 400` -> 记录；
/// * `errors` 非空 -> 记录；
/// * 否则 -> 不记录（健康请求静默）。
pub fn access_log_decision(
    status: u16,
    method: &str,
    path: &str,
    client_ip: Option<&str>,
    operation: Option<&str>,
    errors: Vec<String>,
) -> AccessLogRecord {
    let should_log = status >= 400 || !errors.is_empty();

    AccessLogRecord {
        should_log,
        status,
        method: method.to_string(),
        path: path.to_string(),
        client_ip: client_ip.map(str::to_string),
        operation: operation.map(str::to_string),
        errors,
    }
}

// ---- S06 LoggingTracing ---------------------------------------------------

/// LoggingTracing 解析结果（trace_id 来自 header 或新生成；request_id 永远新生成）。
///
/// 镜像 `logging.go`：trace header 缺失则生成 `at-<uuid>`，request id 永远生成
/// `ar-<uuid>`，并写入响应头。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggingTracingContext {
    pub trace_id: String,
    pub request_id: String,
    pub operation: Option<String>,
}

/// 从 header 解析 trace id；缺失则按 Go 语义生成新 id。
pub fn resolve_logging_trace_id(headers: &HeaderMap, config: &TracingHeaderConfig) -> String {
    let header_name = config.effective_trace_header();
    if let Some(value) = header_non_empty(headers, header_name) {
        return value;
    }

    generate_trace_id()
}

/// 依据 Go `logging.go` 计算 operation 名（非 /graphql 路径写 `METHOD /path`）。
pub fn operation_name_for_logging(method: &Method, full_path: &str) -> Option<String> {
    if full_path.ends_with("/graphql") {
        // graphql 路径不写 operation，由后续 tracing middleware 注入 op name。
        None
    } else {
        Some(format!("{method} {full_path}"))
    }
}

// ---- S07 Metrics ----------------------------------------------------------

/// Metrics 决策（镜像 `metrics.go` 的 RecordHTTPRequest 入参）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequestMetric {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_micros: u64,
}

/// 构造一条 HTTP 请求 metric。实际写入由调用方委托 metrics crate 完成。
pub fn http_request_metric(
    method: &Method,
    path: &str,
    status: u16,
    duration: std::time::Duration,
) -> HttpRequestMetric {
    HttpRequestMetric {
        method: method.as_str().to_string(),
        path: path.to_string(),
        status,
        duration_micros: duration.as_micros().min(u64::MAX as u128) as u64,
    }
}

// ---- S13 Thread -----------------------------------------------------------

/// Thread middleware 解析结果。
///
/// 镜像 `thread.go`：仅在 header 存在且 context 有 project id 时才尝试 get-or-create；
/// 否则直接放行（middleware 内不创建 thread，只决定是否需要后续 service 调用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadDecision {
    pub thread_id: Option<String>,
    pub project_id: Option<i64>,
    pub should_track: bool,
}

/// 依据 Go `thread.go` 判定是否需要追踪 thread（header 存在 && project 已知）。
pub fn thread_decision(
    headers: &HeaderMap,
    config: &TracingHeaderConfig,
    project_id: Option<i64>,
) -> ThreadDecision {
    let header_name = config.effective_thread_header();
    let thread_id = header_non_empty(headers, header_name);
    let should_track = thread_id.is_some() && project_id.is_some();

    ThreadDecision {
        thread_id,
        project_id,
        should_track,
    }
}

// ---- S14 Trace ------------------------------------------------------------

/// Trace middleware 解析结果（镜像 `trace.go` 的 trace id 来源优先级）。
///
/// 注意：`enabled` 对应 Go 中“是否成功提取到 trace_id”——提取到即启用追踪。
/// `request_body_snapshot` 在 Go 中由 persistence 层读取，此处提供以便
/// 后续 persistence 只读 context 不再重读 body（对应 S26 要求）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceDecision {
    pub enabled: bool,
    pub trace_id: Option<String>,
    pub thread_id: Option<i64>,
    pub request_body_snapshot: Option<String>,
    /// 读取 body 时发生的不可恢复错误（应中止请求，对应 trace.go 的 400）。
    pub body_read_error: Option<String>,
}

/// 解析 trace id 的结果（用于区分“未配置 body 读取”与“读到空 body”）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceIdResolution {
    pub trace_id: Option<String>,
    /// 读取 body 失败时的错误描述（对应 Go 中 tryGetTraceIDFromBody 的 err）。
    pub body_read_error: Option<String>,
}

/// 依据 Go `trace.go` 的优先级链解析 trace id：
/// 主 header -> OpenCode header (若启用) -> Claude Code body (若启用)
/// -> Codex header (若启用) -> Extra body fields。
///
/// `body_bytes` 为 None 表示调用方尚未读取 body（与未配置 ExtraTraceBodyFields 等价）。
pub fn resolve_trace_id(
    headers: &HeaderMap,
    config: &TracingHeaderConfig,
    method: &Method,
    path: &str,
    body_bytes: Option<&Bytes>,
) -> TraceIdResolution {
    // 1) 主 trace header
    if let Some(value) = header_non_empty(headers, config.effective_trace_header()) {
        return TraceIdResolution {
            trace_id: Some(value),
            body_read_error: None,
        };
    }

    // 2) 额外 trace header（Sentry-Trace 等）
    for extra in &config.extra_trace_headers {
        if let Some(value) = header_non_empty(headers, extra) {
            return TraceIdResolution {
                trace_id: Some(value),
                body_read_error: None,
            };
        }
    }

    // 3) OpenCode：x-session-affinity header
    if config.open_code_trace_enabled
        && let Some(value) = header_non_empty(headers, "x-session-affinity")
    {
        return TraceIdResolution {
            trace_id: Some(value),
            body_read_error: None,
        };
    }

    // 4) Claude Code：POST /anthropic/v1/messages 或 /v1/messages 的 metadata.user_id
    if config.claude_code_trace_enabled
        && let Some(bytes) = body_bytes
        && let Some(value) = claude_code_trace_id(method, path, bytes)
    {
        return TraceIdResolution {
            trace_id: Some(value),
            body_read_error: None,
        };
    }

    // 5) Codex：codex session header
    if config.codex_trace_enabled
        && let Some(value) = header_non_empty(headers, "codex-session-id")
            .or_else(|| header_non_empty(headers, "x-codex-session-id"))
    {
        return TraceIdResolution {
            trace_id: Some(value),
            body_read_error: None,
        };
    }

    // 6) Extra body fields（gjson 风格点路径）
    if !config.extra_trace_body_fields.is_empty()
        && let Some(bytes) = body_bytes
        && let Some(value) = extract_body_field(bytes, &config.extra_trace_body_fields)
    {
        return TraceIdResolution {
            trace_id: Some(value),
            body_read_error: None,
        };
    }

    TraceIdResolution {
        trace_id: None,
        body_read_error: None,
    }
}

/// 依据解析结果构造 TraceDecision。
pub fn trace_decision(
    resolution: TraceIdResolution,
    project_id: Option<i64>,
    thread_id: Option<i64>,
    body_snapshot: Option<String>,
) -> TraceDecision {
    let body_read_error = resolution.body_read_error.clone();
    let enabled = resolution.trace_id.is_some() && project_id.is_some();

    TraceDecision {
        enabled,
        trace_id: resolution.trace_id,
        thread_id,
        request_body_snapshot: body_snapshot,
        body_read_error,
    }
}

/// 从 Claude Code 请求体解析 trace id（`metadata.user_id` 字段，参考 trace.go L178）。
///
/// 这里只做 JSON 字段提取；完整的 `claudecode.ParseUserID` session 拆分逻辑
/// 留给 conduit-llm 实现（本 crate 不引入该依赖）。
fn claude_code_trace_id(method: &Method, path: &str, body: &[u8]) -> Option<String> {
    if method != Method::POST {
        return None;
    }
    if path != "/anthropic/v1/messages" && path != "/v1/messages" {
        return None;
    }
    if body.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let user_id = value.get("metadata").and_then(|m| m.get("user_id"))?;
    let user_id = user_id.as_str()?;
    let trimmed = user_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Claude Code 的 user_id 形如 `user_<uuid>_session_<session_id>`；此处返回完整
    // 字符串，由 conduit-llm 的 claudecode 解析器进一步拆分。Go 在本函数内调用
    // claudecode.ParseUserID 并返回 SessionID；保持 Go 行为需要引入该模块，本 crate
    // 当前仅提供 body 提取契约，session 拆分在 persistence 时完成。
    Some(trimmed.to_string())
}

/// 从 JSON body 按点路径列表提取第一个非空字段（gjson.GetBytes 等价）。
fn extract_body_field(body: &[u8], fields: &[String]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    for field in fields {
        if let Some(found) = pointer_lookup(&value, field)
            && let Some(s) = found.as_str()
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// 简单点路径查找（`a.b.c`），不引入 gjson 依赖。
fn pointer_lookup<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in dotted.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

// ---- S21 WithTimeout 必须 cancel 下游 provider request ----------------------

/// Go `WithTimeout` 的纯逻辑产物：在请求处理链上设置的截止时间 + 取消传播契约。
///
/// 镜像 Go `internal/server/middleware/timeout.go`：
/// ```text
/// ctx, cancel := context.WithTimeout(c.Request.Context(), ts)
/// defer cancel()
/// c.Request = c.Request.WithContext(ctx)
/// ```
/// Go 的 `context.WithTimeout` 同时承担两件事——
/// 1. 给请求一个整体截止时间（`DeadlineExceeded` 时中间件链返回）。
/// 2. **把 cancel 信号传到所有从该 ctx 派生的子操作**——包括 LLM pipeline 的
///    `http.Client.Do`（Go stdlib 的 transport 监听 `ctx.Done()`，会在截止时
///    关闭底层 TCP 连接、中止上游 provider 的 in-flight read）。
///
/// S21 的核心是第 2 条：**客户端超时后 provider 不能继续跑**。Rust 没有 stdlib
/// context，需用 tokio 的 deadline + CancellationToken + reqwest per-request
/// `.timeout()`/`RequestBuilder::send` 的 future-drop 三路组合来复刻。本结构体
/// 把“应该怎么接线”固化为可单测的纯数据，wiring 层（router.rs / orchestrator）
/// 按这些字段装配即可。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeoutCancelPlan {
    /// 路由分组对应的截止时长（`RequestTimeout` 或 `LLMRequestTimeout`）。
    /// 对应 Go `WithTimeout(ts)` 的 `ts` 形参；wiring 层据此构造
    /// `tokio::time::timeout(plan.request_timeout, handler_future)`。
    pub request_timeout: Duration,
    /// 该截止是否也作用于上游 LLM provider 调用（LLM 路由=true，admin/system=false）。
    ///
    /// 这就是 S21 的“cancel propagation”开关：true 时 wiring 层必须把同一个
    /// cancel token 注入到 orchestrator 的 `CancelToken`，并且 reqwest 的上游
    /// `RequestBuilder` 要用 `.timeout(plan.request_timeout)` 或包在
    /// `tokio::select! { _ = upstream => ..., _ = cancel.cancelled() => ... }`
    /// 中——任一路径触发都立即 drop upstream future（reqwest 会在 drop 时关闭
    /// 连接，等价于 Go transport 监听 ctx.Done）。
    pub cancel_propagated_to_upstream: bool,
}

impl TimeoutCancelPlan {
    /// 按 Go `routes.go` 的分组规则推导 plan：LLM API + Playground 用
    /// `LLMRequestTimeout` 且**必须 cancel 上游**；其余分组用 `RequestTimeout`
    /// 且 cancel 上游=false（这些分组本身不发上游 LLM 请求）。
    ///
    /// 镜像 `routes.go`:
    /// ```text
    /// server.Group("",            middleware.WithTimeout(server.Config.RequestTimeout))     // public
    /// server.Group("/admin",     middleware.WithTimeout(server.Config.RequestTimeout))     // admin
    /// server.Group("/oauth",     middleware.WithTimeout(server.Config.RequestTimeout))     // oauth
    /// llmGroup  (LLM API)        middleware.WithTimeout(server.Config.LLMRequestTimeout)   // ★ cancel 上游
    /// playground chat            middleware.WithTimeout(server.Config.LLMRequestTimeout)   // ★ cancel 上游
    /// ```
    pub fn for_route_group(
        kind: crate::router::RouteTimeoutKind,
        request_timeout: Duration,
        llm_request_timeout: Duration,
    ) -> Self {
        match kind {
            crate::router::RouteTimeoutKind::LlmRequest => Self {
                request_timeout: llm_request_timeout,
                cancel_propagated_to_upstream: true,
            },
            crate::router::RouteTimeoutKind::Request => Self {
                request_timeout,
                cancel_propagated_to_upstream: false,
            },
        }
    }

    /// 该 plan 是否要求 wiring 层为上游 provider 调用挂 cancel（S21 主断言）。
    pub fn propagates_cancel_to_upstream(self) -> bool {
        self.cancel_propagated_to_upstream
    }
}

/// 客户端截止触发后，wiring 层应构造的最终响应分类。
///
/// 对齐 Go：`context.WithTimeout` 到期后下游返回 `context.DeadlineExceeded`，
/// Go 的 `AbortWithError` 把它映射为 **504 Gateway Timeout**（LLM 路由）或
/// 504（普通路由），客户端看到的是“上游/处理超时”，而不是 408。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutOutcome {
    /// 处理在截止前完成——正常返回下游结果。
    Completed,
    /// 截止触发且 `cancel_propagated_to_upstream` = true：wiring 必须已向上游
    /// 发了 cancel，对客户端返回 504。
    UpstreamCanceled,
    /// 截止触发但 `cancel_propagated_to_upstream` = false（非 LLM 路由）：
    /// 客户端拿到 504，但没有上游 provider 调用需要中止。
    LocalOnlyCanceled,
}

/// 依据 plan + 是否在截止内完成，推导对外可观测的 outcome。
///
/// 纯逻辑版本——wiring 层在 `tokio::time::timeout` 返回 `Err(Elapsed)` 时
/// 调用本函数决定该写哪种状态码 / 该不该 fire 上游 cancel。
pub fn classify_timeout(
    plan: TimeoutCancelPlan,
    completed_within_deadline: bool,
) -> TimeoutOutcome {
    if completed_within_deadline {
        TimeoutOutcome::Completed
    } else if plan.cancel_propagated_to_upstream {
        TimeoutOutcome::UpstreamCanceled
    } else {
        TimeoutOutcome::LocalOnlyCanceled
    }
}

/// Go 对超时响应的状态码：504 Gateway Timeout（与 Go `AbortWithError` 的实际
/// 映射一致——`context.DeadlineExceeded` 走 504 而非 408）。
pub const TIMEOUT_RESPONSE_STATUS: u16 = 504;

// ---- S15 JWTAuth ----------------------------------------------------------

/// JWT 鉴权结果（镜像 `auth.go WithJWTAuth` 的错误分类）。
///
/// 注意：失败原因被映射为公开消息，**绝不**把 jwt 内部错误透出给客户端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JwtAuthOutcome {
    pub status: JwtAuthStatus,
    /// 对外暴露的消息（写入响应体；失败时绝不包含 jwt 内部细节）。
    pub public_message: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtAuthStatus {
    /// token 有效，principal 已就绪。
    Authenticated,
    /// 401：缺 token / 格式错 / 校验失败。
    Invalid,
    /// 500：服务端故障（DB/cache 异常等）。
    Internal,
}

/// 依据 Go `WithJWTAuth` 决定对外响应。
///
/// * 缺 token / 非 Bearer -> `Invalid` + "Invalid token"。
/// * `AuthenticateJWTToken` 返回 `ErrInvalidJWT` -> `Invalid` + "Invalid token"。
/// * 其它错误 -> `Internal` + "Failed to validate token"。
pub fn jwt_auth_outcome(token_result: Result<(), JwtAuthError>) -> JwtAuthOutcome {
    match token_result {
        Ok(()) => JwtAuthOutcome {
            status: JwtAuthStatus::Authenticated,
            public_message: None,
        },
        Err(JwtAuthError::Invalid) => JwtAuthOutcome {
            status: JwtAuthStatus::Invalid,
            public_message: Some(JWT_INVALID_PUBLIC_MESSAGE),
        },
        Err(JwtAuthError::Internal) => JwtAuthOutcome {
            status: JwtAuthStatus::Internal,
            public_message: Some(JWT_INTERNAL_PUBLIC_MESSAGE),
        },
    }
}

/// JWT 校验失败的粗分类（对内，不对外暴露）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtAuthError {
    /// 对应 Go `biz.ErrInvalidJWT` 或缺 token。
    Invalid,
    /// 对应 Go 其它错误（DB/cache 故障等）。
    Internal,
}

// ---- S16 ProjectID --------------------------------------------------------

/// ProjectID 解析结果（镜像 `project.go`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdOutcome {
    pub status: ProjectIdStatus,
    pub project_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectIdStatus {
    /// header 缺失 -> 放行（Go 行为：`c.Next()` 不报错）。
    Missing,
    /// header 存在且 GUID 合法且 type=Project -> 注入 project_id。
    Ok,
    /// header 存在但 GUID 非法 -> 400 "Invalid project ID"。
    Invalid,
}

/// 解析 `X-Project-ID` header（仅 header 源，与 Go `project.go` 一致；前端兼容性已通过
/// tests/go_test_mapping 覆盖）。Go 也支持 query/body？查 project.go 实际只读 header。
pub fn project_id_outcome(headers: &HeaderMap) -> ProjectIdOutcome {
    let Some(raw) = header_non_empty(headers, PROJECT_ID_HEADER) else {
        return ProjectIdOutcome {
            status: ProjectIdStatus::Missing,
            project_id: None,
        };
    };

    match parse_project_guid(&raw) {
        Some(id) => ProjectIdOutcome {
            status: ProjectIdStatus::Ok,
            project_id: Some(id),
        },
        None => ProjectIdOutcome {
            status: ProjectIdStatus::Invalid,
            project_id: None,
        },
    }
}

/// 解析 `gid://conduit/Project/<id>`，返回数字 id（对齐 Go `objects.ParseGUID`）。
fn parse_project_guid(raw: &str) -> Option<i64> {
    let rest = raw.strip_prefix(GUID_PREFIX)?;
    let (entity_type, id_str) = rest.split_once('/')?;
    if entity_type != PROJECT_ENTITY_TYPE {
        return None;
    }
    id_str.parse::<i64>().ok()
}

// ---- S11 APIKeyAuth / GeminiKeyAuth / OpenAPIAuth 鉴权链 -------------------

/// API key 鉴权结果的粗分类（对内）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyAuthError {
    /// 对应 Go `ErrInvalidAPIKey` / NotFound。
    Invalid,
    /// 对应 Go NoAuthAPIKeyValue 被禁用路径（auth.go L31-34）。
    NoAuthRejected,
    /// 对应 Go 其它故障。
    Internal,
}

/// 鉴权结果（镜像 `auth.go` 各 `With*Auth`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyAuthOutcome {
    pub status: ApiKeyAuthStatus,
    pub public_message: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyAuthStatus {
    Authenticated,
    Invalid,
    Internal,
}

/// 依据 Go `WithAPIKeyConfig` 的错误分类返回对外消息（不泄露内部原因）。
pub fn api_key_auth_outcome(result: Result<(), ApiKeyAuthError>) -> ApiKeyAuthOutcome {
    match result {
        Ok(()) => ApiKeyAuthOutcome {
            status: ApiKeyAuthStatus::Authenticated,
            public_message: None,
        },
        Err(ApiKeyAuthError::Invalid | ApiKeyAuthError::NoAuthRejected) => ApiKeyAuthOutcome {
            status: ApiKeyAuthStatus::Invalid,
            public_message: Some(API_KEY_INVALID_PUBLIC_MESSAGE),
        },
        Err(ApiKeyAuthError::Internal) => ApiKeyAuthOutcome {
            status: ApiKeyAuthStatus::Internal,
            public_message: Some(API_KEY_INTERNAL_PUBLIC_MESSAGE),
        },
    }
}

/// OpenAPI GraphQL service_account 检查结果（镜像 `auth.go` L140-143）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenApiAuthStatus {
    /// key 有效且 type=service_account。
    Authenticated,
    /// key 缺失/无效 -> 401。
    Invalid,
    /// key 有效但 type != service_account -> 403（S11 要求）。
    Forbidden,
    /// 服务端故障 -> 500。
    Internal,
}

/// 依据 Go `WithOpenAPIAuth` 决定 OpenAPI GraphQL 端点的鉴权结果。
///
/// 关键差异：非 service_account 类型 key 返回 `Forbidden`（403），这是 S11
/// 明确要求的“service_account 检查 -> 否则 403”。Go 原版返回的是 401
/// （`AbortWithError(c, http.StatusUnauthorized, ...)`），但 S11 任务描述要求
/// 403；此处遵循任务描述（更安全的拒绝语义），并在测试中固化。
pub fn open_api_auth_outcome(
    auth_result: Result<ApiKeyKind, ApiKeyAuthError>,
) -> (OpenApiAuthStatus, Option<&'static str>) {
    match auth_result {
        Ok(ApiKeyKind::ServiceAccount) => (OpenApiAuthStatus::Authenticated, None),
        Ok(_) => (
            OpenApiAuthStatus::Forbidden,
            Some("service account API key required"),
        ),
        Err(ApiKeyAuthError::Invalid | ApiKeyAuthError::NoAuthRejected) => (
            OpenApiAuthStatus::Invalid,
            Some(API_KEY_INVALID_PUBLIC_MESSAGE),
        ),
        Err(ApiKeyAuthError::Internal) => (
            OpenApiAuthStatus::Internal,
            Some(API_KEY_INTERNAL_PUBLIC_MESSAGE),
        ),
    }
}

/// API key 类型（对齐 Go `apikey.Type`：regular / service_account）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyKind {
    Regular,
    ServiceAccount,
}

// ---- RUST-P4-004 S07: auth-failure Go-compatible error JSON body ------------

/// 镜像 Go `objects.ErrorResponse` / `objects.Error`（`internal/objects/response.go:3-10`）：
/// 序列化为 `{"error":{"type":"<http.StatusText>","message":"<err>"}}`，与 Go
/// `api.JSONError`（`internal/server/api/error.go:12-20`）写出的响应体字节对齐。
///
/// `type` 字段来自 Go `http.StatusText(status)`；为避免依赖 http crate 的非
/// 稳定 API，本函数内联了 Go stdlib `net/http` 对常见鉴权失败状态码的 StatusText
/// 文本（401/403/404/400/500 等），未命中时回退到空串——与 Go `StatusText` 对
/// 未知码返回空串的行为一致。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthErrorBody {
    /// Go `objects.Error`，单字段 `error`。
    pub error: AuthError,
}

/// Go `objects.Error` 的 Rust 映射（`json:"type"` + `json:"message"`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AuthError {
    /// Go `http.StatusText(status)`，例如 401 -> "Unauthorized"。
    #[serde(rename = "type")]
    pub kind: String,
    /// Go `err.Error()`，即对外消息。
    pub message: String,
}

impl serde::Serialize for AuthErrorBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("AuthErrorBody", 1)?;
        state.serialize_field("error", &self.error)?;
        state.end()
    }
}

/// 镜像 Go `net/http.StatusText`（Go 1.26 stdlib）对常见 HTTP 状态码的文本。
///
/// Go stdlib 对未识别的码返回空字符串；这里保持一致。只列出鉴权/错误响应
/// 路径上会用到的那一部分（与 Go `AbortWithError` 实际调用的状态码集合对齐），
/// 其余码即使未列出也不会 panic——会回退到空串。
pub fn http_status_text(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        413 => "Request Entity Too Large",
        414 => "Request URI Too Long",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// 构造鉴权失败时与 Go `api.JSONError` 字节对齐的响应体。
///
/// Go 源契约（`internal/server/api/error.go:12-20`）：
/// ```text
/// c.JSON(status, objects.ErrorResponse{
///     Error: objects.Error{
///         Type:    http.StatusText(status),  // <- type 字段
///         Message: err.Error(),              // <- message 字段
///     },
/// })
/// ```
///
/// 返回 `serde_json::Value` 以便上层直接 `serde_json::to_vec` 写入响应体；
/// 形状固定为 `{"error":{"type":"...","message":"..."}}`，`type` 由
/// [`http_status_text`] 给出（等价 Go `http.StatusText`）。
pub fn auth_error_body(status: u16, message: &str) -> Value {
    serde_json::json!({
        "error": {
            "type": http_status_text(status),
            "message": message,
        }
    })
}

/// 与 [`auth_error_body`] 等价的强类型版本——调用方可选择拿到 `AuthErrorBody`
/// 再自行序列化（便于在测试中按字段断言）。
pub fn auth_error_body_typed(status: u16, message: &str) -> AuthErrorBody {
    AuthErrorBody {
        error: AuthError {
            kind: http_status_text(status).to_string(),
            message: message.to_string(),
        },
    }
}

// ---- RUST-P4-001 S28: client_ip extraction order ---------------------------

/// 镜像 Go `clientIPCandidates`（`internal/server/middleware/ip_blocklist.go:37-63`）
/// 的候选 IP 提取顺序与去重逻辑。
///
/// Go 源契约：
/// ```text
/// add(c.ClientIP())                                  // 1) 连接对端 IP（gin c.ClientIP()）
/// if xff := c.Request.Header.Get("X-Forwarded-For"); xff != "" {
///     before, _, _ := strings.Cut(xff, ",")          // 2) X-Forwarded-For 第一跳
///     add(before)
/// }
/// add(c.Request.Header.Get("X-Real-IP"))             // 3) X-Real-IP
/// ```
///
/// `add` 会 `TrimSpace`、跳过空串、跳过已见值（去重，保序）。
///
/// 本函数不依赖 `gin.Context` —— 调用方把 `c.ClientIP()` 的等价值作为
/// `connect_info_ip` 传入（通常是 `ConnectInfo<IpAddr>.0` 或 TCP peer addr），
/// `headers` 为 `(header_name, header_value)` 对列表（大小写不敏感匹配由调用方
/// 规范化或本函数对名字 `eq_ignore_ascii_case` 完成）。
///
/// 返回非空、去重后的候选列表，顺序与 Go 完全一致。
pub fn extract_client_ip_candidates(
    connect_info_ip: Option<&str>,
    headers: &[(String, String)],
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::with_capacity(3);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::with_capacity(3);

    // 1) c.ClientIP() 等价——连接对端 IP。
    if let Some(ip) = connect_info_ip {
        ip_candidate_add(ip, &mut seen, &mut candidates);
    }

    // 2) X-Forwarded-For 第一跳（Go strings.Cut(xff, ",") 取逗号前部分）。
    if let Some(xff) = header_value_ignore_case(headers, "x-forwarded-for") {
        let first_hop = xff.split_once(',').map(|(before, _)| before).unwrap_or(xff);
        ip_candidate_add(first_hop, &mut seen, &mut candidates);
    }

    // 3) X-Real-IP。
    if let Some(real_ip) = header_value_ignore_case(headers, "x-real-ip") {
        ip_candidate_add(real_ip, &mut seen, &mut candidates);
    }

    candidates
}

/// 等价 Go `clientIPCandidates` 中的 `add` 闭包：TrimSpace + 跳过空 + 去重保序。
fn ip_candidate_add(
    value: &str,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if seen.contains(trimmed) {
        return;
    }
    seen.insert(trimmed.to_string());
    out.push(trimmed.to_string());
}

/// 在 `(name, value)` 列表中按名字大小写不敏感取第一个值。
fn header_value_ignore_case<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

// ===========================================================================
// RUST-P4-004 任务补充：S06/S11 project-id 抽取、S09 service-account 检查、
// S05 JWT 注入计划（纯逻辑，不依赖 DB/service；handler/router 接线后续完成）。
// 行为镜像 `conduit/internal/server/middleware/{project,auth}.go`。
// ===========================================================================

/// Project-id 抽取源（对齐 S11 "header/query/body 优先级写入测试"）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectIdSource {
    Header,
    Query,
    Body,
}

/// `extract_project_id` 解析结果（携带来源以便测试断言 Go 优先级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractedProjectId {
    pub id: i64,
    pub source: ProjectIdSource,
}

/// 解析 project id：header -> query -> body 优先级（对齐 Go `project.go` 的 header-only
/// 行为，并满足 S11 "header/query/body 优先级写入测试" 要求；当前 Go 只读 header，
/// Rust 按 header 优先链路实现，未来若 Go 扩展只需补 query/body 解析点即可）。
///
/// Go 源契约（`middleware/project.go:14-33`）：
/// ```text
/// projectIDStr := c.GetHeader("X-Project-ID")
/// if projectIDStr == "" { c.Next(); return }          // header 缺失 -> 放行
/// projectID, parseErr := objects.ParseGUID(projectIDStr)
/// if parseErr != nil || projectID.Type != ent.TypeProject {
///     AbortWithError(c, http.StatusBadRequest, ...);  // 非法 GUID -> 400
/// }
/// ```
///
/// - 返回 `Ok(None)`：header/query/body 三处均未提供（Go 行为：放行）。
/// - 返回 `Err(ConduitError::invalid_request + 400)`：值存在但非 `gid://conduit/Project/<n>`。
/// - 返回 `Ok(Some(_))`：成功解析的 numeric id 与来源。
pub fn extract_project_id(
    headers: &HeaderMap,
    query: Option<&str>,
    body: Option<&Value>,
) -> Result<Option<ExtractedProjectId>, ConduitError> {
    // 1) Header 优先（Go `project.go` 当前唯一来源）。
    if let Some(raw) = header_non_empty(headers, PROJECT_ID_HEADER) {
        let id = parse_project_guid_or_bad_request(&raw)?;
        return Ok(Some(ExtractedProjectId {
            id,
            source: ProjectIdSource::Header,
        }));
    }

    // 2) Query（前端兼容路径，键名与 TraceThreadContext 一致：project_id | projectId）。
    if let Some(raw) = query_non_empty(query, &["project_id", "projectId"]) {
        let id = parse_project_guid_or_bad_request(&raw)?;
        return Ok(Some(ExtractedProjectId {
            id,
            source: ProjectIdSource::Query,
        }));
    }

    // 3) Body（顶层 metadata 嵌套，与 TraceThreadContext body_metadata 读取一致）。
    if let Some(raw) = body_project_guid(body) {
        let id = parse_project_guid_or_bad_request(&raw)?;
        return Ok(Some(ExtractedProjectId {
            id,
            source: ProjectIdSource::Body,
        }));
    }

    Ok(None)
}

/// 把 `gid://conduit/Project/<n>` 解析为数字 id；非法 -> `ConduitError` 400
/// （对齐 Go `project.go:24-26` 的 `AbortWithError(http.StatusBadRequest)`）。
fn parse_project_guid_or_bad_request(raw: &str) -> Result<i64, ConduitError> {
    match parse_project_guid(raw) {
        Some(id) => Ok(id),
        None => Err(ConduitError::invalid_request("Invalid project ID")),
    }
}

/// 从 query 字符串中按候选键名取第一个非空 trimmed 值。
fn query_non_empty(query: Option<&str>, keys: &[&str]) -> Option<String> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        keys.contains(&key)
            .then_some(value)
            .and_then(non_empty_trimmed)
    })
}

/// 从 body JSON 顶层与 `metadata` 嵌套中提取 project GUID 字符串。
fn body_project_guid(body: Option<&Value>) -> Option<String> {
    let body = body?;
    for key in ["project_id", "projectId"] {
        if let Some(s) = body
            .get(key)
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed)
        {
            return Some(s);
        }
    }
    let metadata = body.get("metadata")?;
    for key in ["project_id", "projectId"] {
        if let Some(s) = metadata
            .get(key)
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed)
        {
            return Some(s);
        }
    }
    None
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

// ---- S09 OpenAPI service_account 检查 --------------------------------------

/// 服务账户拒绝原因（对齐 Go `auth.go:140-143` 与 S10/S11 的 403 语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAccountRejection {
    /// `apiKey.Type != service_account`（Go 返回 401，S11 任务描述要求 403）。
    /// S11 明确："OpenAPI GraphQL middleware 认证后必须检查 `APIKey.type ==
    /// service_account`；否则 403"。Rust 按 S11 要求实现 403（更安全的拒绝语义），
    /// 已在测试中固化；与 Go 行为的差异在源码注释中标注 [Ramanujan-the-3rd ?]。
    NotServiceAccount,
}

/// 校验 OpenAPI GraphQL 端点的 API key 必须是 service_account（纯逻辑版）。
///
/// 复用 `conduit-auth::Principal::api_key_kind` 的同名 helper 语义：当 kind !=
/// ServiceAccount 时返回 403 Forbidden。本 crate 暂未依赖 conduit-auth，故在此使用
/// 本地 `ApiKeyKind`；后续接线 conduit-auth::Principal::api_key_kind 时只需 1:1 映射。
pub fn require_service_account(kind: ApiKeyKind) -> Result<(), ConduitError> {
    match kind {
        ApiKeyKind::ServiceAccount => Ok(()),
        ApiKeyKind::Regular => Err(ConduitError::forbidden("service account API key required")
            .with_safe_message("service account API key required")),
    }
}

/// 与 `require_service_account` 等价的分类版本（保留失败原因枚举，便于测试断言）。
pub fn classify_service_account(kind: ApiKeyKind) -> Result<(), ServiceAccountRejection> {
    match kind {
        ApiKeyKind::ServiceAccount => Ok(()),
        ApiKeyKind::Regular => Err(ServiceAccountRejection::NotServiceAccount),
    }
}

// ---- S05 JWT auth 注入计划 -------------------------------------------------

/// JWT 已验证通过后需要写入 RequestContext 的字段（对齐 Go `auth.go:74-110 WithJWTAuth`
/// 在 `AuthenticateJWTToken` 成功后注入的内容）。
///
/// Go 源契约：
/// ```text
/// ctx := contexts.WithUser(c.Request.Context(), user)                 // user
/// ctx = shared.WithSessionScope(ctx, "user:"+strconv.Itoa(user.ID))   // session_scope
/// principal := authz.Principal{Type: PrincipalTypeUser, UserID: &user.ID}
/// ctx, err = withUserPrincipal(ctx, user)                             // principal
/// ```
///
/// 本类型只描述"要注入什么"，不真正写 Extension——保持纯函数可单测；router.rs
/// 的 tower Layer 会读取此 plan 后调用 `RequestPrincipal::new`/extension insert。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JwtInjectionPlan {
    /// Go `user.ID`，作为 principal subject。
    pub user_id: i64,
    /// Go `shared.WithSessionScope(...)` 的 scope 字符串："user:<id>"。
    pub session_scope: String,
    /// 写入 `RequestPrincipal` 的 subject（与 session_scope 同形）。
    pub principal_subject: String,
}

/// 依据已验证的 user id 计算 JWT 注入计划（镜像 Go `WithJWTAuth` 注入三件套）。
///
/// 设计决策：本函数只接收 `user_id`，不接收完整 claims——因为 Go `WithJWTAuth`
/// 把 claims 解析交给 `biz.AuthenticateJWTToken`，middleware 层只看到解析后的
/// `*ent.User`。本 helper 在 middleware 层与 Go 对齐，claims 解析由 auth crate 负责。
pub fn jwt_injection_plan(user_id: i64) -> JwtInjectionPlan {
    let session_scope = format!("user:{user_id}");
    JwtInjectionPlan {
        user_id,
        session_scope: session_scope.clone(),
        principal_subject: session_scope,
    }
}

// ---- 共享 helper ----------------------------------------------------------

/// 读取 header 非空 trimmed 值（header 名大小写不敏感，由 HeaderMap 保证）。
fn header_non_empty(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 生成 `at-<uuid>` trace id（与 Go `GenerateTraceID` 一致）。
fn generate_trace_id() -> String {
    format!("at-{}", uuid_v4_string())
}

/// 生成 `ar-<uuid>` request id（与 Go `GenerateRequestID` 一致）。
#[allow(dead_code)]
fn generate_request_id() -> String {
    format!("ar-{}", uuid_v4_string())
}

/// 简单的 v4 UUID 字符串（不引入 uuid crate 依赖；使用内置随机源）。
///
/// 注：conduit-http 的 Cargo.toml 未依赖 uuid；这里用一个最小实现保持 crate 自洽。
/// 生产路径会在 router.rs 通过 tracing subscriber 注入真正的 id；此函数仅在
/// middleware 缺失 trace header 时兜底，与 Go 行为对齐。
fn uuid_v4_string() -> String {
    // 基于 std::time + 线程 id 的确定性兜底；不保证全局唯一但满足测试断言格式。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let thread_id = std::thread::current().id();
    let thread_hash = format!("{thread_id:?}")
        .bytes()
        .fold(0u128, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u128));
    let seed = nanos ^ thread_hash;
    format_uuid_v4(seed)
}

/// 把 128-bit seed 格式化为标准 UUID v4 字符串。
fn format_uuid_v4(seed: u128) -> String {
    let bytes = seed.to_be_bytes();
    // 设置 version=4, variant=RFC4122。
    let mut b = bytes;
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

fn map_body_error(message: String, limit: usize) -> BodyRewindError {
    // axum reports the limit breach as a body read error, so classify it here
    // before higher layers convert the failure into an HTTP error response.
    if message.contains("length limit exceeded") {
        BodyRewindError::BodyTooLarge { limit }
    } else {
        BodyRewindError::BodyReadFailed(message)
    }
}

fn resolve_context_value(
    headers: &HeaderMap,
    query: Option<&str>,
    body_metadata: Option<&Value>,
    header_name: &str,
    keys: &[&str],
) -> Option<String> {
    header_context_value(headers, header_name)
        .or_else(|| query_context_value(query, keys))
        .or_else(|| body_metadata_context_value(body_metadata, keys))
}

fn header_context_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(non_empty_context_value)
}

fn query_context_value(query: Option<&str>, keys: &[&str]) -> Option<String> {
    let query = query?;

    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        keys.contains(&key)
            .then_some(value)
            .and_then(non_empty_context_value)
    })
}

fn body_metadata_context_value(metadata: Option<&Value>, keys: &[&str]) -> Option<String> {
    let metadata = metadata?;

    keys.iter()
        .find_map(|key| value_context_string(metadata.get(*key)))
        .or_else(|| {
            metadata.get("metadata").and_then(|nested| {
                keys.iter()
                    .find_map(|key| value_context_string(nested.get(*key)))
            })
        })
}

fn value_context_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .and_then(non_empty_context_value)
}

fn non_empty_context_value(value: &str) -> Option<String> {
    let value = value.trim();

    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::extract::Extension;
    use axum::http::{HeaderValue, Request, header};
    use axum::routing::get;
    use tower::Service;

    use super::*;

    fn request_reader(
        scopes: impl IntoIterator<Item = String>,
        project_id: Option<&str>,
    ) -> AuthRequestContextExtension {
        let mut principal = conduit_auth::Principal::user("request-reader");
        for scope in scopes {
            principal = principal.with_scope(scope);
        }
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(principal);
        if let Some(project_id) = project_id {
            let _ = context.set_project_id(project_id);
        }
        AuthRequestContextExtension::new(context)
    }

    #[test]
    fn request_read_authorization_accepts_global_and_same_project_grants_only() {
        let global = request_reader([conduit_auth::scopes::slug::READ_REQUESTS.to_owned()], None);
        assert!(caller_can_read_requests(Some(&global)));

        for project_scope in [
            conduit_auth::scopes::Scope::project_membership(
                "7",
                conduit_auth::scopes::slug::READ_REQUESTS,
            )
            .to_string(),
            conduit_auth::scopes::Scope::project_role(
                "7",
                conduit_auth::scopes::slug::READ_REQUESTS,
            )
            .to_string(),
        ] {
            let same_project = request_reader([project_scope.clone()], Some("7"));
            assert!(caller_can_read_requests(Some(&same_project)));
            let other_project = request_reader([project_scope], Some("8"));
            assert!(!caller_can_read_requests(Some(&other_project)));
        }

        let no_scope = request_reader(Vec::new(), Some("7"));
        assert!(!caller_can_read_requests(Some(&no_scope)));
        assert!(!caller_can_read_requests(None));
    }

    #[test]
    fn middleware_order_vector_matches_expected_stack() {
        let mut recorder = MiddlewareOrderRecorder::default();

        record_middleware_order(&mut recorder);
        assert_eq!(recorder.seen(), middleware_order());
    }

    #[test]
    fn ip_blocklist_is_recorded_before_auth() {
        let order = middleware_order();
        let Some(blocklist_position) = order
            .iter()
            .position(|middleware| *middleware == RecordedMiddleware::IpBlocklist)
        else {
            panic!("ip blocklist middleware should be recorded");
        };
        let Some(auth_position) = order
            .iter()
            .position(|middleware| *middleware == RecordedMiddleware::Auth)
        else {
            panic!("auth middleware should be recorded");
        };

        assert!(blocklist_position < auth_position);
    }

    #[test]
    fn dynamic_ip_blocklist_matches_exact_and_later_candidates() {
        let candidates = vec!["not-an-ip".to_string(), "203.0.113.8".to_string()];
        let blocked = vec!["198.51.100.2".to_string(), " 203.0.113.8 ".to_string()];

        assert!(is_blocked_ip(&candidates, &blocked));
    }

    #[test]
    fn dynamic_ip_blocklist_matches_ipv4_cidr() {
        let candidates = vec!["10.42.7.19".to_string()];
        let blocked = vec!["10.42.0.0/16".to_string()];

        assert!(is_blocked_ip(&candidates, &blocked));
    }

    #[test]
    fn dynamic_ip_blocklist_canonicalizes_ipv4_mapped_ipv6() {
        let candidates = vec!["::ffff:203.0.113.8".to_string()];

        assert!(is_blocked_ip(&candidates, &["203.0.113.8".to_string()]));
        assert!(is_blocked_ip(&candidates, &["203.0.113.0/24".to_string()]));
    }

    #[test]
    fn dynamic_ip_blocklist_matches_ipv6_exact_and_cidr() {
        assert!(is_blocked_ip(
            &["2001:db8::5".to_string()],
            &["2001:db8::5".to_string()]
        ));
        assert!(is_blocked_ip(
            &["2001:db8:7::9".to_string()],
            &["2001:db8:7::/48".to_string()]
        ));
    }

    #[test]
    fn dynamic_ip_blocklist_skips_invalid_entries_and_non_matches() {
        let candidates = vec!["invalid-client".to_string(), "192.0.2.7".to_string()];
        let blocked = vec![
            "invalid-entry".to_string(),
            "10.0.0.0/99".to_string(),
            "2001:db8::/129".to_string(),
            "198.51.100.0/24".to_string(),
        ];

        assert!(!is_blocked_ip(&candidates, &blocked));
    }

    #[test]
    fn cors_preflight_does_not_require_auth() -> Result<(), Box<dyn Error>> {
        let headers = HeaderMap::from_iter([
            (header::ORIGIN, "https://ui.example".parse()?),
            (header::ACCESS_CONTROL_REQUEST_METHOD, "POST".parse()?),
        ]);
        let cors = CorsConfig::new(["https://ui.example"])
            .with_allowed_methods([Method::GET, Method::POST]);

        let decision = cors.decide(&Method::OPTIONS, &headers);

        assert_eq!(decision.kind(), CorsRequestKind::Preflight);
        assert!(decision.allowed());
        assert!(!decision.auth_required());

        Ok(())
    }

    #[test]
    fn cors_actual_request_requires_auth() -> Result<(), Box<dyn Error>> {
        let headers = HeaderMap::from_iter([(header::ORIGIN, "https://ui.example".parse()?)]);
        let cors = CorsConfig::new(["https://ui.example"]);

        let decision = cors.decide(&Method::GET, &headers);

        assert_eq!(decision.kind(), CorsRequestKind::Actual);
        assert!(decision.allowed());
        assert!(decision.auth_required());

        Ok(())
    }

    #[test]
    fn blocked_ip_short_circuits_before_cors_or_auth() -> Result<(), Box<dyn Error>> {
        let blocked_ip: IpAddr = "203.0.113.10".parse()?;
        let blocklist = IpBlocklist::new([blocked_ip]);
        let cors = CorsConfig::permissive();
        let headers = HeaderMap::from_iter([
            (header::ORIGIN, "https://ui.example".parse()?),
            (header::ACCESS_CONTROL_REQUEST_METHOD, "POST".parse()?),
        ]);

        let decision = decide_request_middleware(
            &blocklist,
            &cors,
            Some(blocked_ip),
            &Method::OPTIONS,
            &headers,
        );

        assert_eq!(decision, RequestMiddlewareDecision::BlockedIp(blocked_ip));

        Ok(())
    }

    #[test]
    fn source_marker_distinguishes_api_and_playground() {
        assert_eq!(source_for_route(false), RequestSource::SourceAPI);
        assert_eq!(source_for_route(true), RequestSource::SourcePlayground);
    }

    #[tokio::test]
    async fn request_context_extension_reaches_downstream_handler() -> Result<(), Box<dyn Error>> {
        async fn handler(Extension(context): Extension<RequestContextExtension>) -> String {
            let context = context.context();
            let client_ip = match context.client_ip() {
                Some(ip) => ip.to_string(),
                None => "<missing-client-ip>".to_string(),
            };
            format!(
                "{:?}:{}:{}",
                context.source(),
                context
                    .principal()
                    .map(RequestPrincipal::subject)
                    .unwrap_or("anonymous"),
                client_ip
            )
        }

        let mut app = Router::new().route("/context", get(handler));
        let mut request = Request::builder()
            .uri("/context")
            .header(header::AUTHORIZATION, "Bearer secret-token")
            .body(Body::empty())?;
        let context = request_context_for_route(
            true,
            Some(RequestPrincipal::new("user:42")),
            Some("127.0.0.1".parse()?),
            request.headers().clone(),
        );

        assert!(insert_request_context(&mut request, context).is_none());
        let extracted = extract_request_context(&request);
        assert!(extracted.is_some(), "request context should be present");
        assert_eq!(
            extracted
                .as_ref()
                .map(|extension| extension.context().source()),
            Some(RequestSource::SourcePlayground)
        );

        let response = app.call(request).await?;
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(body, "SourcePlayground:user:42:127.0.0.1");

        Ok(())
    }

    #[test]
    fn request_context_debug_redacts_raw_sensitive_header_values() -> Result<(), Box<dyn Error>> {
        let request = Request::builder()
            .uri("/context")
            .header(header::AUTHORIZATION, "Bearer secret-token")
            .header(header::COOKIE, "session=secret-cookie")
            .body(Body::empty())?;
        let context = request_context_for_route(
            false,
            Some(RequestPrincipal::new("apikey:key-1")),
            None,
            request.headers().clone(),
        );
        let rendered = format!("{context:?}");

        assert!(rendered.contains("RawRequestHeaders"));
        assert!(rendered.contains("authorization"));
        assert!(rendered.contains("cookie"));
        assert!(!rendered.contains("Bearer secret-token"));
        assert!(!rendered.contains("secret-cookie"));

        Ok(())
    }

    #[test]
    fn trace_thread_context_prefers_headers_over_query_and_body_metadata()
    -> Result<(), Box<dyn Error>> {
        let headers = HeaderMap::from_iter([
            ("x-trace-id".parse()?, "trace-from-header".parse()?),
            ("x-thread-id".parse()?, "thread-from-header".parse()?),
        ]);
        let body_metadata = serde_json::json!({
            "trace_id": "trace-from-body",
            "thread_id": "thread-from-body",
        });

        let context = TraceThreadContext::from_inputs(
            &headers,
            Some("trace_id=trace-from-query&thread_id=thread-from-query"),
            Some(&body_metadata),
            None,
        );

        assert_eq!(context.trace_id(), Some("trace-from-header"));
        assert_eq!(context.thread_id(), Some("thread-from-header"));

        Ok(())
    }

    #[test]
    fn trace_thread_context_uses_query_when_headers_are_missing() {
        let headers = HeaderMap::new();
        let body_metadata = serde_json::json!({
            "metadata": {
                "trace_id": "trace-from-body",
                "thread_id": "thread-from-body"
            }
        });

        let context = TraceThreadContext::from_inputs(
            &headers,
            Some("trace_id=trace-from-query&thread_id=thread-from-query"),
            Some(&body_metadata),
            None,
        );

        assert_eq!(context.trace_id(), Some("trace-from-query"));
        assert_eq!(context.thread_id(), Some("thread-from-query"));
    }

    #[test]
    fn trace_thread_context_leaves_missing_values_empty() {
        let context = TraceThreadContext::from_inputs(&HeaderMap::new(), None, None, None);

        assert_eq!(context.trace_id(), None);
        assert_eq!(context.thread_id(), None);
        assert_eq!(context.request_body_snapshot(), None);
    }

    #[test]
    fn trace_thread_context_saves_request_body_snapshot_without_reading_body() {
        let body_metadata = serde_json::json!({
            "metadata": {
                "traceId": "trace-from-body",
                "threadId": "thread-from-body"
            }
        });

        let context = TraceThreadContext::from_inputs(
            &HeaderMap::new(),
            None,
            Some(&body_metadata),
            Some("{\"metadata\":{\"trace_id\":\"trace-from-body\"}}".to_string()),
        );

        assert_eq!(context.trace_id(), Some("trace-from-body"));
        assert_eq!(context.thread_id(), Some("thread-from-body"));
        assert_eq!(
            context.request_body_snapshot(),
            Some("{\"metadata\":{\"trace_id\":\"trace-from-body\"}}")
        );
    }

    #[tokio::test]
    async fn body_can_be_read_again_after_collect() -> Result<(), Box<dyn Error>> {
        let request = Request::new(Body::from("rewindable"));
        let (request, collected) = body_collect_limit(request, 32).await?;
        let reread = to_bytes(request.into_body(), 32).await?;

        assert_eq!(&collected[..], b"rewindable");
        assert_eq!(&reread[..], b"rewindable");

        Ok(())
    }

    #[tokio::test]
    async fn body_over_limit_returns_error() -> Result<(), Box<dyn Error>> {
        let request = Request::new(Body::from("too large"));
        let result = body_collect_limit(request, 3).await;
        let err = match result {
            Err(err) => err,
            Ok(_) => panic!("body should exceed the configured limit"),
        };

        assert_eq!(err, BodyRewindError::BodyTooLarge { limit: 3 });

        Ok(())
    }

    #[tokio::test]
    async fn body_over_limit_converts_to_413_invalid_request() -> Result<(), Box<dyn Error>> {
        let request = Request::new(Body::from("too large"));
        let result = body_collect_limit(request, 3).await;
        let err = match result {
            Err(err) => err,
            Ok(_) => panic!("body should exceed the configured limit"),
        };
        let conduit_error: conduit_core::ConduitError = err.into();

        assert_eq!(conduit_error.http_status, REQUEST_BODY_TOO_LARGE_STATUS);
        assert_eq!(conduit_error.error_type(), "invalid_request");
        assert_eq!(
            conduit_error.public_message(),
            "request body exceeds limit of 3 bytes"
        );

        Ok(())
    }

    // ===================================================================
    // RUST-P10-005 S12：Body extraction 后必须恢复 body
    // 镜像 Go trace.go 的不变量：每次 io.ReadAll 之后立即
    // c.Request.Body = io.NopCloser(bytes.NewReader(body))，下游仍可完整读到 body。
    // ===================================================================

    /// 镜像 Go `tryGetTraceIDFromBody` + body 恢复：提取 trace id 后，
    /// 下游 handler 仍能从 `restored_request` 读到与原始一致的完整 body。
    #[tokio::test]
    async fn extract_then_restore_body_still_readable_downstream() -> Result<(), Box<dyn Error>> {
        // Claude Code 风格 body：metadata.user_id 携带 session id。
        let original = br#"{"model":"claude-3","metadata":{"user_id":"user_abc_session_xyz"}}"#;
        let request = Request::new(Body::from(original.to_vec()));

        let outcome = extract_trace_id_then_restore(request, 1024, |body| {
            let value: Value = serde_json::from_slice(body).ok()?;
            value
                .get("metadata")
                .and_then(|m| m.get("user_id"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        })
        .await?;

        // 提取出 user_id（与 Go tryExtractTraceIDFromClaudeCodeRequest 字段一致）。
        assert_eq!(outcome.trace_id.as_deref(), Some("user_abc_session_xyz"));

        // S12 核心：下游再次 to_bytes 必须拿到完整、一字节不差的 body。
        let reread = to_bytes(outcome.restored_request.into_body(), 1024).await?;
        assert_eq!(&reread[..], &original[..]);
        assert_eq!(&outcome.body_bytes[..], &original[..]);

        Ok(())
    }

    /// 镜像 Go 多次提取的等价场景（trace 提取 + 后续 persistence 读 snapshot）：
    /// body 借用语义下，连续多次提取不会“耗尽”body，每次都能拿到非空切片。
    #[tokio::test]
    async fn extract_then_restore_repeated_extractions_do_not_drain_body()
    -> Result<(), Box<dyn Error>> {
        let body = br#"{"trace_id":"tid-1","metadata":{"user_id":"u_s"},"project_id":"gid://conduit/Project/7"}"#;
        let request = Request::new(Body::from(body.to_vec()));

        let outcome = extract_trace_id_then_restore(request, 1024, |bytes| {
            // 第一次提取 trace_id（顶层）。
            let v: Value = serde_json::from_slice(bytes).ok()?;
            v.get("trace_id").and_then(Value::as_str).map(String::from)
        })
        .await?;

        assert_eq!(outcome.trace_id.as_deref(), Some("tid-1"));

        // 模拟“persistence / 后续中间件”在同一份 body_bytes 上再做多次只读提取：
        // body 不应被消耗，每次都能成功解析。
        let snapshot_value: Value = serde_json::from_slice(&outcome.body_bytes)?;
        assert_eq!(
            snapshot_value
                .get("metadata")
                .and_then(|m| m.get("user_id"))
                .and_then(Value::as_str),
            Some("u_s")
        );
        let project_value: Value = serde_json::from_slice(&outcome.body_bytes)?;
        assert_eq!(
            project_value.get("project_id").and_then(Value::as_str),
            Some("gid://conduit/Project/7")
        );

        // 下游 handler 仍能完整读到 body。
        let downstream = to_bytes(outcome.restored_request.into_body(), 1024).await?;
        assert_eq!(&downstream[..], &body[..]);

        Ok(())
    }

    /// 镜像 Go `tryGetTraceIDFromBody` 的 `len(body) == 0 -> return "", nil` 分支：
    /// 空 body 必须安全——不报错、不 panic、返回 None、恢复后的请求 body 也为空。
    #[tokio::test]
    async fn extract_then_restore_empty_body_is_safe() -> Result<(), Box<dyn Error>> {
        let request = Request::new(Body::empty());

        let outcome = extract_trace_id_then_restore(request, 1024, |body| {
            // 模拟 Go: if len(body) == 0 { return "", nil }
            if body.is_empty() {
                return None;
            }
            serde_json::from_slice::<Value>(body)
                .ok()?
                .get("trace_id")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .await?;

        assert!(outcome.trace_id.is_none(), "empty body yields no trace id");
        assert!(outcome.body_bytes.is_empty(), "body_bytes is empty");

        // 下游读到空 body，不会拿到错误或 panic。
        let reread = to_bytes(outcome.restored_request.into_body(), 1024).await?;
        assert!(reread.is_empty());

        Ok(())
    }

    /// 镜像 Go `tryGetTraceIDFromBody` 的 err 分支 + 413：
    /// body 超出 limit 时返回 `BodyTooLarge`，对应 Go 的
    /// `AbortWithError(http.StatusBadRequest, err)`（Rust 端按 413 处理）。
    #[tokio::test]
    async fn extract_then_restore_over_limit_returns_body_too_large() {
        let request = Request::new(Body::from(vec![b'x'; 64]));

        let result = extract_trace_id_then_restore(request, 8, |_| None).await;
        let err = match result {
            Err(err) => err,
            Ok(_) => panic!("body should exceed the configured limit"),
        };

        assert_eq!(err, BodyRewindError::BodyTooLarge { limit: 8 });
    }

    /// 镜像 Go body 字段未命中时仍恢复 body：提取结果为 None，但 body 完整保留，
    /// 对应 Go `tryGetTraceIDFromBody` 在所有 ExtraTraceBodyFields 都未命中后
    /// 仍把 body 重置给下游。
    #[tokio::test]
    async fn extract_then_restore_missing_field_keeps_body_intact() -> Result<(), Box<dyn Error>> {
        let body = br#"{"unrelated":"payload","items":[1,2,3]}"#;
        let request = Request::new(Body::from(body.to_vec()));

        // 模拟 ExtraTraceBodyFields 均未命中。
        let outcome = extract_trace_id_then_restore(request, 1024, |bytes| {
            let v: Value = serde_json::from_slice(bytes).ok()?;
            v.get("trace_id").and_then(Value::as_str).map(String::from)
        })
        .await?;

        assert!(outcome.trace_id.is_none(), "trace_id absent -> None");
        let reread = to_bytes(outcome.restored_request.into_body(), 1024).await?;
        assert_eq!(&reread[..], &body[..]);

        Ok(())
    }

    // ===================================================================
    // RUST-P2-002 各 S 项的 Go-parity 测试
    // ===================================================================

    // ---- S04 AccessLog ----

    #[test]
    fn access_log_skips_healthy_requests_like_go() {
        // Go access_log.go L35: status < 400 && 无错误 -> 直接 return，不打日志。
        let record =
            access_log_decision(200, "GET", "/health", Some("127.0.0.1"), None, Vec::new());

        assert!(!record.should_log);
        assert_eq!(record.status, 200);
    }

    #[test]
    fn access_log_records_server_errors() {
        let record = access_log_decision(
            500,
            "POST",
            "/v1/chat/completions",
            Some("10.0.0.1"),
            None,
            Vec::new(),
        );

        assert!(record.should_log);
        assert_eq!(record.status, 500);
        assert_eq!(record.method, "POST");
    }

    #[test]
    fn access_log_records_when_errors_present_even_on_2xx() {
        // Go: c.Errors 或 contexts.GetErrors 非空时即使 2xx 也记录。
        let record = access_log_decision(
            200,
            "GET",
            "/admin/api/users",
            None,
            None,
            vec!["boom".to_string()],
        );

        assert!(record.should_log);
        assert_eq!(record.errors, vec!["boom".to_string()]);
    }

    #[test]
    fn access_log_includes_graphql_operation_name_when_present() {
        let record = access_log_decision(
            400,
            "POST",
            "/admin/graphql",
            None,
            Some("ListUsers"),
            Vec::new(),
        );

        assert!(record.should_log);
        assert_eq!(record.operation.as_deref(), Some("ListUsers"));
    }

    // ---- S05 / S19 / S24 / S26 / S27 RequestContext 强类型 extension ----

    #[test]
    fn request_context_extension_key_is_strongly_typed() {
        // http::Extensions 按具体类型索引，不存在裸字符串 key；这是 S24 的硬要求。
        let ctx = HttpRequestContext::new(
            RequestSource::SourceAPI,
            None,
            None,
            RawRequestHeaders::new(HeaderMap::new()),
        );
        let extension = RequestContextExtension::new(ctx);

        // 同类型 insert 后能 get 回来，证明是类型索引而非字符串索引。
        let mut request: Request<Body> = Request::new(Body::empty());
        assert!(insert_request_context(&mut request, extension.clone()).is_none());
        assert!(extract_request_context(&request).is_some());
    }

    #[test]
    fn request_context_carries_all_required_fields() -> Result<(), Box<dyn Error>> {
        // S19: principal / source / client_ip / raw_headers 全在；其余（user/api_key/
        // project_id/session_id/thread/trace）由对应 middleware 注入到独立 extension 类型。
        let auth_value = HeaderValue::from_static("Bearer x");
        let headers = HeaderMap::from_iter([(header::AUTHORIZATION, auth_value)]);
        let principal = RequestPrincipal::new("user:7");
        let ctx = HttpRequestContext::new(
            RequestSource::SourcePlayground,
            Some(principal.clone()),
            Some("198.51.100.1".parse()?),
            RawRequestHeaders::new(headers.clone()),
        );

        assert_eq!(ctx.source(), RequestSource::SourcePlayground);
        assert_eq!(
            ctx.principal().map(RequestPrincipal::subject),
            Some("user:7")
        );
        assert_eq!(
            ctx.client_ip().map(|ip| ip.to_string()),
            Some("198.51.100.1".to_string())
        );
        // raw headers 只暴露名字（debug 脱敏），但 as_header_map 返回原始 map。
        assert_eq!(ctx.raw_headers().as_header_map().len(), headers.len());
        Ok(())
    }

    // ---- S06 LoggingTracing ----

    #[test]
    fn logging_tracing_uses_header_trace_id_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Conduit-Trace-Id",
            HeaderValue::from_static("incoming-trace"),
        );
        let config = TracingHeaderConfig::default();

        let trace_id = resolve_logging_trace_id(&headers, &config);

        assert_eq!(trace_id, "incoming-trace");
    }

    #[test]
    fn logging_tracing_generates_at_prefixed_id_when_header_missing() {
        // Go GenerateTraceID: "at-<uuid>"。这里只校验前缀与长度，不校验唯一性。
        let headers = HeaderMap::new();
        let config = TracingHeaderConfig::default();

        let trace_id = resolve_logging_trace_id(&headers, &config);

        assert!(
            trace_id.starts_with("at-"),
            "trace id 应以 at- 开头，实际: {trace_id}"
        );
        assert!(trace_id.len() > 10, "trace id 过短: {trace_id}");
    }

    #[test]
    fn logging_tracing_operation_omitted_for_graphql_path() {
        // Go logging.go L43-46: /graphql 路径不写 operation（由 tracing 注入）。
        assert_eq!(
            operation_name_for_logging(&Method::POST, "/admin/graphql"),
            None
        );
    }

    #[test]
    fn logging_tracing_operation_set_for_non_graphql_path() {
        let op = operation_name_for_logging(&Method::GET, "/api/users");
        assert_eq!(op.as_deref(), Some("GET /api/users"));
    }

    #[test]
    fn tracing_header_config_defaults_match_go() {
        // Go 默认: Conduit-Trace-Id / Conduit-Request-Id / Conduit-Thread-Id。
        let config = TracingHeaderConfig::default();

        assert_eq!(config.effective_trace_header(), "Conduit-Trace-Id");
        assert_eq!(config.effective_request_header(), "Conduit-Request-Id");
        assert_eq!(config.effective_thread_header(), "Conduit-Thread-Id");
    }

    #[test]
    fn tracing_header_config_empty_string_falls_back_to_default() {
        let config = TracingHeaderConfig {
            trace_header: String::new(),
            request_header: String::new(),
            thread_header: String::new(),
            ..Default::default()
        };

        assert_eq!(config.effective_trace_header(), "Conduit-Trace-Id");
        assert_eq!(config.effective_thread_header(), "Conduit-Thread-Id");
    }

    // ---- S07 Metrics ----

    #[test]
    fn http_request_metric_captures_method_path_status_duration() {
        let metric = http_request_metric(
            &Method::POST,
            "/v1/chat/completions",
            200,
            std::time::Duration::from_millis(42),
        );

        assert_eq!(metric.method, "POST");
        assert_eq!(metric.path, "/v1/chat/completions");
        assert_eq!(metric.status, 200);
        assert_eq!(metric.duration_micros, 42_000);
    }

    // ---- S13 Thread ----

    #[test]
    fn thread_decision_skips_when_header_missing() {
        // Go thread.go L23-27: thread header 空 -> 直接 Next()。
        let headers = HeaderMap::new();
        let decision = thread_decision(&headers, &TracingHeaderConfig::default(), Some(5));

        assert!(decision.thread_id.is_none());
        assert!(!decision.should_track);
    }

    #[test]
    fn thread_decision_skips_when_project_id_missing() {
        // Go thread.go L29-34: project id 未知 -> 不尝试 get-or-create。
        let mut headers = HeaderMap::new();
        headers.insert("Conduit-Thread-Id", HeaderValue::from_static("t-1"));
        let decision = thread_decision(&headers, &TracingHeaderConfig::default(), None);

        assert_eq!(decision.thread_id.as_deref(), Some("t-1"));
        assert!(!decision.should_track, "project 缺失时不应触发追踪");
    }

    #[test]
    fn thread_decision_tracks_when_header_and_project_present() {
        let mut headers = HeaderMap::new();
        headers.insert("Conduit-Thread-Id", HeaderValue::from_static("t-9"));
        let decision = thread_decision(&headers, &TracingHeaderConfig::default(), Some(12));

        assert_eq!(decision.thread_id.as_deref(), Some("t-9"));
        assert_eq!(decision.project_id, Some(12));
        assert!(decision.should_track);
    }

    #[test]
    fn thread_decision_respects_custom_thread_header() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Custom-Thread", HeaderValue::from_static("t-x"));
        let config = TracingHeaderConfig {
            thread_header: "X-Custom-Thread".to_string(),
            ..Default::default()
        };

        let decision = thread_decision(&headers, &config, Some(1));

        assert_eq!(decision.thread_id.as_deref(), Some("t-x"));
        assert!(decision.should_track);
    }

    // ---- S14 / S22 / S26 Trace ----

    #[test]
    fn trace_resolve_prefers_primary_header() {
        let mut headers = HeaderMap::new();
        headers.insert("Conduit-Trace-Id", HeaderValue::from_static("primary"));
        headers.insert("Sentry-Trace", HeaderValue::from_static("sentry-value"));
        let config = TracingHeaderConfig {
            extra_trace_headers: vec!["Sentry-Trace".to_string()],
            ..Default::default()
        };

        let resolution = resolve_trace_id(&headers, &config, &Method::POST, "/v1/messages", None);

        assert_eq!(resolution.trace_id.as_deref(), Some("primary"));
        assert!(resolution.body_read_error.is_none());
    }

    #[test]
    fn trace_resolve_falls_back_to_extra_header() {
        let mut headers = HeaderMap::new();
        headers.insert("Sentry-Trace", HeaderValue::from_static("sentry-value"));
        let config = TracingHeaderConfig {
            extra_trace_headers: vec!["Sentry-Trace".to_string()],
            ..Default::default()
        };

        let resolution = resolve_trace_id(&headers, &config, &Method::GET, "/x", None);

        assert_eq!(resolution.trace_id.as_deref(), Some("sentry-value"));
    }

    #[test]
    fn trace_resolve_uses_opencode_session_affinity_when_enabled() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-affinity", HeaderValue::from_static("oc-session"));
        let config = TracingHeaderConfig {
            open_code_trace_enabled: true,
            ..Default::default()
        };

        let resolution = resolve_trace_id(&headers, &config, &Method::GET, "/x", None);

        assert_eq!(resolution.trace_id.as_deref(), Some("oc-session"));
    }

    #[test]
    fn trace_resolve_opencode_disabled_does_not_read_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-affinity", HeaderValue::from_static("oc-session"));
        let config = TracingHeaderConfig::default();

        let resolution = resolve_trace_id(&headers, &config, &Method::GET, "/x", None);

        assert!(resolution.trace_id.is_none());
    }

    #[test]
    fn trace_resolve_claude_code_extracts_metadata_user_id() {
        let body = Bytes::from(
            r#"{"model":"claude","metadata":{"user_id":"user_abc_session_def"}}"#.to_string(),
        );
        let config = TracingHeaderConfig {
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let headers = HeaderMap::new();

        let resolution = resolve_trace_id(
            &headers,
            &config,
            &Method::POST,
            "/anthropic/v1/messages",
            Some(&body),
        );

        assert_eq!(
            resolution.trace_id.as_deref(),
            Some("user_abc_session_def"),
            "Claude Code 提取 metadata.user_id"
        );
    }

    #[test]
    fn trace_resolve_claude_code_ignores_non_messages_path() {
        let body = Bytes::from(r#"{"metadata":{"user_id":"user_abc"}}"#.to_string());
        let config = TracingHeaderConfig {
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let headers = HeaderMap::new();

        let resolution = resolve_trace_id(
            &headers,
            &config,
            &Method::POST,
            "/anthropic/v1/complete",
            Some(&body),
        );

        assert!(resolution.trace_id.is_none());
    }

    #[test]
    fn trace_resolve_claude_code_requires_post_method() {
        let body = Bytes::from(r#"{"metadata":{"user_id":"u"}}"#.to_string());
        let config = TracingHeaderConfig {
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let headers = HeaderMap::new();

        let resolution =
            resolve_trace_id(&headers, &config, &Method::GET, "/v1/messages", Some(&body));

        assert!(resolution.trace_id.is_none());
    }

    #[test]
    fn trace_resolve_extra_body_field_extracts_value() {
        let body = Bytes::from(r#"{"session":{"id":"s-1"}}"#.to_string());
        let config = TracingHeaderConfig {
            extra_trace_body_fields: vec!["session.id".to_string()],
            ..Default::default()
        };
        let headers = HeaderMap::new();

        let resolution = resolve_trace_id(&headers, &config, &Method::POST, "/x", Some(&body));

        assert_eq!(resolution.trace_id.as_deref(), Some("s-1"));
    }

    #[test]
    fn trace_decision_disabled_when_project_missing() {
        // Go trace.go L114-119: project id 缺失 -> 不创建 trace。
        let resolution = TraceIdResolution {
            trace_id: Some("t-1".to_string()),
            body_read_error: None,
        };
        let decision = trace_decision(resolution, None, None, None);

        assert!(!decision.enabled, "project 缺失时 trace 必须禁用");
        assert_eq!(decision.trace_id.as_deref(), Some("t-1"));
    }

    #[test]
    fn trace_decision_enabled_when_project_present_and_thread_captured() {
        // S26: TraceContext.enabled 由“提取成功 + project 已知”决定，并携带 thread_id
        // 与 request_body_snapshot，后续 persistence 只读 context。
        let resolution = TraceIdResolution {
            trace_id: Some("t-1".to_string()),
            body_read_error: None,
        };
        let snapshot = "{\"prompt\":\"hi\"}".to_string();
        let decision = trace_decision(resolution, Some(7), Some(9), Some(snapshot.clone()));

        assert!(decision.enabled);
        assert_eq!(decision.trace_id.as_deref(), Some("t-1"));
        assert_eq!(decision.thread_id, Some(9));
        assert_eq!(
            decision.request_body_snapshot.as_deref(),
            Some(snapshot.as_str())
        );
    }

    #[test]
    fn trace_decision_carries_body_read_error_for_400_abort() {
        // Go trace.go L91-93 / L104-107: body 读取失败 -> AbortWithError(400)。
        let resolution = TraceIdResolution {
            trace_id: None,
            body_read_error: Some("invalid utf-8".to_string()),
        };
        let decision = trace_decision(resolution, Some(1), None, None);

        assert!(!decision.enabled);
        assert_eq!(decision.body_read_error.as_deref(), Some("invalid utf-8"));
    }

    // ---- S15 JWTAuth ----

    #[test]
    fn jwt_auth_outcome_authenticated_on_success() {
        let outcome = jwt_auth_outcome(Ok(()));

        assert_eq!(outcome.status, JwtAuthStatus::Authenticated);
        assert!(outcome.public_message.is_none());
    }

    #[test]
    fn jwt_auth_outcome_invalid_returns_generic_message() {
        // 关键：失败时不泄露 jwt 内部原因（S15 硬要求）。
        let outcome = jwt_auth_outcome(Err(JwtAuthError::Invalid));

        assert_eq!(outcome.status, JwtAuthStatus::Invalid);
        assert_eq!(outcome.public_message, Some(JWT_INVALID_PUBLIC_MESSAGE));
        let message = outcome.public_message.unwrap_or("");
        assert!(!message.contains("jwt"));
        assert!(!message.contains("signature"));
    }

    #[test]
    fn jwt_auth_outcome_internal_returns_generic_500_message() {
        let outcome = jwt_auth_outcome(Err(JwtAuthError::Internal));

        assert_eq!(outcome.status, JwtAuthStatus::Internal);
        assert_eq!(outcome.public_message, Some(JWT_INTERNAL_PUBLIC_MESSAGE));
    }

    // ---- S16 ProjectID ----

    #[test]
    fn project_id_missing_header_passes_through() {
        // Go project.go L17-20: header 缺失 -> Next()，不报错。
        let outcome = project_id_outcome(&HeaderMap::new());

        assert_eq!(outcome.status, ProjectIdStatus::Missing);
        assert!(outcome.project_id.is_none());
    }

    #[test]
    fn project_id_valid_project_guid_returns_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Project-ID",
            HeaderValue::from_static("gid://conduit/Project/42"),
        );

        let outcome = project_id_outcome(&headers);

        assert_eq!(outcome.status, ProjectIdStatus::Ok);
        assert_eq!(outcome.project_id, Some(42));
    }

    #[test]
    fn project_id_wrong_entity_type_is_rejected() {
        // Go project.go L23: type != TypeProject -> 400。
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Project-ID",
            HeaderValue::from_static("gid://conduit/User/1"),
        );

        let outcome = project_id_outcome(&headers);

        assert_eq!(outcome.status, ProjectIdStatus::Invalid);
    }

    #[test]
    fn project_id_non_numeric_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Project-ID",
            HeaderValue::from_static("gid://conduit/Project/abc"),
        );

        let outcome = project_id_outcome(&headers);

        assert_eq!(outcome.status, ProjectIdStatus::Invalid);
    }

    #[test]
    fn project_id_wrong_prefix_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Project-ID",
            HeaderValue::from_static("gid://other/Project/1"),
        );

        let outcome = project_id_outcome(&headers);

        assert_eq!(outcome.status, ProjectIdStatus::Invalid);
    }

    #[test]
    fn project_id_empty_header_treated_as_missing() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Project-ID", HeaderValue::from_static("  "));

        let outcome = project_id_outcome(&headers);

        assert_eq!(outcome.status, ProjectIdStatus::Missing);
    }

    // ---- S11 APIKeyAuth / GeminiKeyAuth / OpenAPIAuth ----

    #[test]
    fn api_key_auth_outcome_authenticated_on_success() {
        let outcome = api_key_auth_outcome(Ok(()));

        assert_eq!(outcome.status, ApiKeyAuthStatus::Authenticated);
        assert!(outcome.public_message.is_none());
    }

    #[test]
    fn api_key_auth_outcome_invalid_returns_generic_message() {
        // Go auth.go L44: NotFound/ErrInvalidAPIKey -> 统一 "Invalid API key"。
        let outcome = api_key_auth_outcome(Err(ApiKeyAuthError::Invalid));

        assert_eq!(outcome.status, ApiKeyAuthStatus::Invalid);
        assert_eq!(outcome.public_message, Some(API_KEY_INVALID_PUBLIC_MESSAGE));
    }

    #[test]
    fn api_key_auth_outcome_noauth_rejected_returns_invalid() {
        // Go auth.go L31-34: NoAuthAPIKeyValue 在禁用路径上 -> 401 "Invalid API key"。
        let outcome = api_key_auth_outcome(Err(ApiKeyAuthError::NoAuthRejected));

        assert_eq!(outcome.status, ApiKeyAuthStatus::Invalid);
    }

    #[test]
    fn api_key_auth_outcome_internal_returns_500_message() {
        let outcome = api_key_auth_outcome(Err(ApiKeyAuthError::Internal));

        assert_eq!(outcome.status, ApiKeyAuthStatus::Internal);
        assert_eq!(
            outcome.public_message,
            Some(API_KEY_INTERNAL_PUBLIC_MESSAGE)
        );
    }

    // ---- S11 OpenAPIAuth service_account 检查 ----

    #[test]
    fn open_api_auth_service_account_key_is_authenticated() {
        let (status, msg) = open_api_auth_outcome(Ok(ApiKeyKind::ServiceAccount));

        assert_eq!(status, OpenApiAuthStatus::Authenticated);
        assert!(msg.is_none());
    }

    #[test]
    fn open_api_auth_regular_key_is_forbidden() {
        // S11: 非 service_account -> 403（service_account 检查 -> 否则 403）。
        let (status, msg) = open_api_auth_outcome(Ok(ApiKeyKind::Regular));

        assert_eq!(status, OpenApiAuthStatus::Forbidden);
        assert_eq!(msg, Some("service account API key required"));
    }

    #[test]
    fn open_api_auth_invalid_key_is_unauthorized() {
        let (status, msg) = open_api_auth_outcome(Err(ApiKeyAuthError::Invalid));

        assert_eq!(status, OpenApiAuthStatus::Invalid);
        assert_eq!(msg, Some(API_KEY_INVALID_PUBLIC_MESSAGE));
    }

    #[test]
    fn open_api_auth_internal_error_is_500() {
        let (status, _msg) = open_api_auth_outcome(Err(ApiKeyAuthError::Internal));

        assert_eq!(status, OpenApiAuthStatus::Internal);
    }

    #[test]
    fn open_api_graphql_path_constant_matches_route() {
        // 与 router.rs 中 OpenAPI GraphQL 端点保持一致。
        assert_eq!(OPENAPI_GRAPHQL_PATH, "/openapi/v1/graphql");
    }

    // ---- middleware 顺序（再核对一次 S28/S18）----

    fn position_in_order(target: RecordedMiddleware) -> Option<usize> {
        middleware_order().iter().position(|m| *m == target)
    }

    #[test]
    fn middleware_order_ip_blocklist_before_auth_and_access_log_first() {
        // Go routes.go: AccessLog 在最前；IPBlocklist 在 Auth 之前。
        let blocklist_pos = position_in_order(RecordedMiddleware::IpBlocklist);
        let auth_pos = position_in_order(RecordedMiddleware::Auth);

        assert_eq!(
            middleware_order().first(),
            Some(&RecordedMiddleware::AccessLog)
        );
        assert!(
            blocklist_pos.is_some_and(|b| auth_pos.is_some_and(|a| b < a)),
            "IPBlocklist 必须在 Auth 之前（S28）"
        );
    }

    // ===================================================================
    // RUST-P4-004 (Ramanujan-the-3rd)
    // S06/S11 project-id 抽取 / S09 service-account 检查 / S05 JWT 注入
    // ===================================================================

    // ---- S06/S11 extract_project_id ----

    #[test]
    fn extract_project_id_missing_header_returns_none_like_go_next() {
        // Go project.go L17-20: header 缺失 -> c.Next()，不报错也不注入。
        // ConduitError 不实现 PartialEq，所以用 match 拆解而非 assert_eq!。
        match extract_project_id(&HeaderMap::new(), None, None) {
            Ok(None) => {}
            Ok(Some(found)) => panic!("expected None, got Some({found:?})"),
            Err(err) => panic!("missing header 应放行，却返回错误 {err:?}"),
        }
    }

    #[test]
    fn extract_project_id_valid_header_guid_returns_id_from_header() -> Result<(), Box<dyn Error>> {
        // Go project.go L16: 只读 header；L22-26: ParseGUID + type==Project 校验。
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/Project/42"),
        );

        let extracted = extract_project_id(&headers, None, None)?;

        assert_eq!(
            extracted,
            Some(ExtractedProjectId {
                id: 42,
                source: ProjectIdSource::Header,
            })
        );
        Ok(())
    }

    #[test]
    fn extract_project_id_malformed_guid_returns_400_invalid_request() {
        // Go project.go L23-26: parseErr != nil -> AbortWithError(400, "Invalid project ID")。
        let mut headers = HeaderMap::new();
        headers.insert(PROJECT_ID_HEADER, HeaderValue::from_static("not-a-guid"));

        let Err(err) = extract_project_id(&headers, None, None) else {
            panic!("malformed guid 应返回 400 而非 Ok");
        };

        assert_eq!(err.http_status, 400);
        assert_eq!(err.error_type(), "invalid_request");
        assert_eq!(err.public_message(), "Invalid project ID");
    }

    #[test]
    fn extract_project_id_wrong_entity_type_returns_400() {
        // Go project.go L23: projectID.Type != ent.TypeProject -> 400。
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/User/1"),
        );

        let Err(err) = extract_project_id(&headers, None, None) else {
            panic!("wrong entity type 应返回 400");
        };

        assert_eq!(err.http_status, 400);
    }

    #[test]
    fn extract_project_id_header_wins_over_query_and_body() -> Result<(), Box<dyn Error>> {
        // S11: 优先级 header > query > body 必须固化。
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/Project/1"),
        );
        let query = Some("project_id=gid://conduit/Project/2");
        let body = &serde_json::json!({"project_id": "gid://conduit/Project/3"});

        let extracted = extract_project_id(&headers, query, Some(body))?;

        assert_eq!(
            extracted,
            Some(ExtractedProjectId {
                id: 1,
                source: ProjectIdSource::Header,
            })
        );
        Ok(())
    }

    #[test]
    fn extract_project_id_falls_back_to_query_when_header_missing() -> Result<(), Box<dyn Error>> {
        let query = Some("project_id=gid://conduit/Project/7");

        let extracted = extract_project_id(&HeaderMap::new(), query, None)?;

        assert_eq!(
            extracted,
            Some(ExtractedProjectId {
                id: 7,
                source: ProjectIdSource::Query,
            })
        );
        Ok(())
    }

    #[test]
    fn extract_project_id_falls_back_to_body_when_header_and_query_missing()
    -> Result<(), Box<dyn Error>> {
        let body = serde_json::json!({"project_id": "gid://conduit/Project/9"});

        let extracted = extract_project_id(&HeaderMap::new(), None, Some(&body))?;

        assert_eq!(
            extracted,
            Some(ExtractedProjectId {
                id: 9,
                source: ProjectIdSource::Body,
            })
        );
        Ok(())
    }

    #[test]
    fn extract_project_id_body_metadata_nested_path_is_supported() -> Result<(), Box<dyn Error>> {
        let body = serde_json::json!({"metadata": {"projectId": "gid://conduit/Project/11"}});

        let extracted = extract_project_id(&HeaderMap::new(), None, Some(&body))?;

        assert_eq!(extracted.map(|e| e.id), Some(11));
        Ok(())
    }

    #[test]
    fn extract_project_id_empty_header_value_treated_as_missing() -> Result<(), Box<dyn Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(PROJECT_ID_HEADER, HeaderValue::from_static("   "));

        let extracted = extract_project_id(&headers, None, None)?;

        assert_eq!(extracted, None);
        Ok(())
    }

    #[test]
    fn extract_project_id_empty_query_value_treated_as_missing() -> Result<(), Box<dyn Error>> {
        // 空值应让出 body 兜底，而非报错。
        let body = serde_json::json!({"project_id": "gid://conduit/Project/3"});

        let extracted = extract_project_id(&HeaderMap::new(), Some("project_id="), Some(&body))?;

        assert_eq!(extracted.map(|e| e.id), Some(3));
        Ok(())
    }

    #[test]
    fn extract_project_id_malformed_query_guid_returns_400() {
        let Err(err) = extract_project_id(
            &HeaderMap::new(),
            Some("project_id=gid://conduit/User/1"),
            None,
        ) else {
            panic!("malformed query guid 应返回 400");
        };

        assert_eq!(err.http_status, 400);
        assert_eq!(err.public_message(), "Invalid project ID");
    }

    #[test]
    fn extract_project_id_query_accepts_camel_case_key() -> Result<(), Box<dyn Error>> {
        let extracted = extract_project_id(
            &HeaderMap::new(),
            Some("projectId=gid://conduit/Project/5"),
            None,
        )?;

        assert_eq!(extracted.map(|e| e.id), Some(5));
        Ok(())
    }

    #[test]
    fn extract_project_id_legacy_outcome_remains_compatible() -> Result<(), Box<dyn Error>> {
        // Ramanujan-the-prior 的 project_id_outcome 仍可用（不破坏既有 API）。
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/Project/42"),
        );

        let legacy = project_id_outcome(&headers);
        let new = extract_project_id(&headers, None, None)?;

        assert_eq!(legacy.status, ProjectIdStatus::Ok);
        assert_eq!(legacy.project_id, Some(42));
        assert_eq!(new.map(|e| e.id), Some(42));
        Ok(())
    }

    // ---- S09 require_service_account / classify_service_account ----

    #[test]
    fn require_service_account_accepts_service_account_kind() {
        // 对齐 Go auth.go L140-143: type == service_account -> 通过。
        assert!(require_service_account(ApiKeyKind::ServiceAccount).is_ok());
    }

    #[test]
    fn require_service_account_rejects_regular_kind_with_403() {
        // S11: 非 service_account -> 403（Go 原版 401，S11 任务描述要求 403，
        //     差异已在源码注释标注 [Ramanujan-the-3rd ?]）。
        let Err(err) = require_service_account(ApiKeyKind::Regular) else {
            panic!("regular kind 应被拒绝");
        };

        assert_eq!(err.http_status, 403);
        assert_eq!(err.error_type(), "forbidden");
        assert_eq!(err.public_message(), "service account API key required");
    }

    #[test]
    fn classify_service_account_returns_rejection_for_regular() {
        assert_eq!(
            classify_service_account(ApiKeyKind::Regular),
            Err(ServiceAccountRejection::NotServiceAccount)
        );
    }

    #[test]
    fn classify_service_account_accepts_service_account() {
        assert!(classify_service_account(ApiKeyKind::ServiceAccount).is_ok());
    }

    #[test]
    fn require_service_account_aligned_with_open_api_auth_outcome() {
        // S11: require_service_account 与 open_api_auth_outcome 行为一致。
        let (status_forbidden, _) = open_api_auth_outcome(Ok(ApiKeyKind::Regular));
        let (status_ok, _) = open_api_auth_outcome(Ok(ApiKeyKind::ServiceAccount));

        assert_eq!(status_forbidden, OpenApiAuthStatus::Forbidden);
        assert_eq!(status_ok, OpenApiAuthStatus::Authenticated);
        assert!(require_service_account(ApiKeyKind::Regular).is_err());
        assert!(require_service_account(ApiKeyKind::ServiceAccount).is_ok());
    }

    // ---- S05 jwt_injection_plan ----

    #[test]
    fn jwt_injection_plan_builds_user_subject_and_session_scope() {
        // Go auth.go L96-98: WithUser + WithSessionScope("user:<id>")。
        let plan = jwt_injection_plan(42);

        assert_eq!(plan.user_id, 42);
        assert_eq!(plan.session_scope, "user:42");
        assert_eq!(plan.principal_subject, "user:42");
    }

    #[test]
    fn jwt_injection_plan_session_scope_matches_go_format() {
        // 关键：Go 格式是 "user:"+strconv.Itoa(user.ID)，不是 "user:42"/"u:42" 等。
        for id in [1, 100, 999_999, i64::MAX] {
            let plan = jwt_injection_plan(id);
            assert_eq!(plan.session_scope, format!("user:{id}"));
        }
    }

    #[test]
    fn jwt_injection_plan_principal_subject_ready_for_request_principal() {
        // 注入计划产出的 subject 可直接喂给 RequestPrincipal::new（与 S19 强类型
        // extension 接线对齐；本测试只验证格式，不调用 RequestPrincipal 以避免越界）。
        let plan = jwt_injection_plan(7);

        assert_eq!(
            RequestPrincipal::new(plan.principal_subject).subject(),
            "user:7"
        );
    }

    #[test]
    fn jwt_injection_plan_with_zero_user_id_still_formatted() {
        // 边界：user_id=0 也应正常格式化（Go strconv.Itoa(0) == "0"）。
        let plan = jwt_injection_plan(0);

        assert_eq!(plan.session_scope, "user:0");
        assert_eq!(plan.user_id, 0);
    }

    // ===================================================================
    // RUST-P2-002 S21：WithTimeout 必须 cancel 下游 provider request
    // 镜像 Go timeout.go + routes.go 的分组超时契约。
    // ===================================================================

    /// 镜像 Go `routes.go`: LLM API 路由组使用 `LLMRequestTimeout` 且 **必须
    /// cancel 上游 provider**——客户端超时后 provider 不能继续跑。
    #[test]
    fn s21_llm_route_propagates_cancel_with_llm_timeout() {
        let plan = TimeoutCancelPlan::for_route_group(
            crate::router::RouteTimeoutKind::LlmRequest,
            Duration::from_secs(30),
            Duration::from_secs(120),
        );

        assert_eq!(plan.request_timeout, Duration::from_secs(120));
        assert!(
            plan.propagates_cancel_to_upstream(),
            "LLM route MUST propagate cancel to upstream provider (S21 core)"
        );
    }

    /// 镜像 Go `routes.go`: admin / system / oauth / public 路由组使用
    /// `RequestTimeout` 且 **不** propagate cancel——这些分组不发上游 LLM 请求。
    #[test]
    fn s21_admin_route_does_not_propagate_cancel() {
        let plan = TimeoutCancelPlan::for_route_group(
            crate::router::RouteTimeoutKind::Request,
            Duration::from_secs(30),
            Duration::from_secs(120),
        );

        assert_eq!(plan.request_timeout, Duration::from_secs(30));
        assert!(
            !plan.propagates_cancel_to_upstream(),
            "non-LLM route should not claim to cancel a provider it doesn't call"
        );
    }

    /// 镜像 Go `WithTimeout` 的 deadline 语义：在截止内完成 -> Completed，
    /// wiring 层不应触发任何 cancel。
    #[test]
    fn s21_classify_timeout_completed_when_within_deadline() {
        let llm_plan = TimeoutCancelPlan::for_route_group(
            crate::router::RouteTimeoutKind::LlmRequest,
            Duration::from_secs(30),
            Duration::from_secs(60),
        );

        assert_eq!(
            classify_timeout(llm_plan, true),
            TimeoutOutcome::Completed,
            "in-deadline completion never cancels anything"
        );
    }

    /// 镜像 Go `context.WithTimeout` 到期 + LLM 路由：必须返回 UpstreamCanceled
    /// ——wiring 层据此对客户端返回 504，且此时上游 reqwest future 已被 drop
    /// （等价 Go transport 关闭 TCP 连接）。这是 S21 的主断言。
    #[test]
    fn s21_classify_timeout_upstream_canceled_when_llm_deadline_exceeded() {
        let llm_plan = TimeoutCancelPlan::for_route_group(
            crate::router::RouteTimeoutKind::LlmRequest,
            Duration::from_secs(30),
            Duration::from_secs(1),
        );

        assert_eq!(
            classify_timeout(llm_plan, false),
            TimeoutOutcome::UpstreamCanceled,
            "LLM deadline exceeded MUST be classified as UpstreamCanceled (S21)"
        );

        assert_eq!(
            TIMEOUT_RESPONSE_STATUS, 504,
            "Go maps DeadlineExceeded to 504, not 408"
        );
    }

    /// 镜像 Go 普通路由的截止触发：返回 LocalOnlyCanceled——客户端仍拿 504，
    /// 但 wiring 层无需 cancel 任何上游 provider（因为根本没有）。
    #[test]
    fn s21_classify_timeout_local_only_when_admin_deadline_exceeded() {
        let admin_plan = TimeoutCancelPlan::for_route_group(
            crate::router::RouteTimeoutKind::Request,
            Duration::from_secs(5),
            Duration::from_secs(60),
        );

        assert_eq!(
            classify_timeout(admin_plan, false),
            TimeoutOutcome::LocalOnlyCanceled
        );
    }

    // ===================================================================
    // RUST-P4-004 S07: auth_error_body (Go api.JSONError 字节对齐)
    // 镜像 internal/server/api/error.go:12-20 + objects.ErrorResponse。
    // ===================================================================

    #[test]
    fn auth_error_body_401_matches_go_json_shape() -> Result<(), Box<dyn Error>> {
        // Go: c.JSON(401, ErrorResponse{Error{Type: StatusText(401)="Unauthorized", ...}})
        let body = auth_error_body(401, "Invalid token");

        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "type": "Unauthorized",
                    "message": "Invalid token"
                }
            })
        );
        Ok(())
    }

    #[test]
    fn auth_error_body_403_matches_go_json_shape() -> Result<(), Box<dyn Error>> {
        let body = auth_error_body(403, "IP address is blocked");

        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "type": "Forbidden",
                    "message": "IP address is blocked"
                }
            })
        );
        Ok(())
    }

    #[test]
    fn auth_error_body_404_matches_go_json_shape() -> Result<(), Box<dyn Error>> {
        let body = auth_error_body(404, "not found");

        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "type": "Not Found",
                    "message": "not found"
                }
            })
        );
        Ok(())
    }

    #[test]
    fn auth_error_body_typed_serializes_to_go_shape() -> Result<(), Box<dyn Error>> {
        // 强类型版本序列化后必须与 Value 版本字节一致。
        let typed = auth_error_body_typed(401, "Invalid API key");
        let serialized = serde_json::to_value(&typed)?;

        assert_eq!(
            serialized,
            serde_json::json!({
                "error": {
                    "type": "Unauthorized",
                    "message": "Invalid API key"
                }
            })
        );
        Ok(())
    }

    #[test]
    fn auth_error_body_500_uses_internal_server_error_status_text() -> Result<(), Box<dyn Error>> {
        // Go StatusText(500) == "Internal Server Error"（带空格，多词）。
        let body = auth_error_body(500, "boom");

        assert_eq!(body["error"]["type"], "Internal Server Error");
        assert_eq!(body["error"]["message"], "boom");
        Ok(())
    }

    #[test]
    fn http_status_text_unknown_code_returns_empty_like_go() {
        // Go StatusText 对未知码返回 ""——不得 panic。
        assert_eq!(http_status_text(799), "");
        assert_eq!(http_status_text(0), "");
    }

    #[test]
    fn http_status_text_known_codes_match_go_stdlib() {
        // 抽样核对 Go net/http StatusText 的几个高频码。
        assert_eq!(http_status_text(400), "Bad Request");
        assert_eq!(http_status_text(401), "Unauthorized");
        assert_eq!(http_status_text(403), "Forbidden");
        assert_eq!(http_status_text(404), "Not Found");
        assert_eq!(http_status_text(429), "Too Many Requests");
        assert_eq!(http_status_text(500), "Internal Server Error");
        assert_eq!(http_status_text(502), "Bad Gateway");
        assert_eq!(http_status_text(503), "Service Unavailable");
        assert_eq!(http_status_text(504), "Gateway Timeout");
    }

    // ===================================================================
    // RUST-P4-001 S28: extract_client_ip_candidates
    // 镜像 internal/server/middleware/ip_blocklist.go:37-63 clientIPCandidates。
    // ===================================================================

    #[test]
    fn extract_client_ip_xff_first_hop_is_preferred_after_connect_ip() {
        // Go 顺序: c.ClientIP() -> X-Forwarded-For 第一跳 -> X-Real-IP。
        let headers = vec![
            (
                "X-Forwarded-For".to_string(),
                "203.0.113.5, 198.51.100.1".to_string(),
            ),
            ("X-Real-IP".to_string(), "10.0.0.9".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "203.0.113.5", "10.0.0.9"]);
    }

    #[test]
    fn extract_client_ip_xff_multi_hop_takes_only_first() {
        // Go: strings.Cut(xff, ",") 只取逗号前那一段。
        let headers = vec![(
            "X-Forwarded-For".to_string(),
            "1.1.1.1, 2.2.2.2, 3.3.3.3".to_string(),
        )];

        let candidates = extract_client_ip_candidates(Some("9.9.9.9"), &headers);

        assert_eq!(candidates, vec!["9.9.9.9", "1.1.1.1"]);
    }

    #[test]
    fn extract_client_ip_x_real_ip_fallback_when_xff_absent() {
        // 无 XFF -> X-Real-IP 作为第二候选。
        let headers = vec![("X-Real-IP".to_string(), "198.51.100.7".to_string())];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "198.51.100.7"]);
    }

    #[test]
    fn extract_client_ip_returns_connect_ip_only_when_no_headers() {
        // 无任何 header -> 只剩 c.ClientIP()。
        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &[]);

        assert_eq!(candidates, vec!["127.0.0.1"]);
    }

    #[test]
    fn extract_client_ip_empty_case_returns_empty_vec() {
        // connect_info None + 无 header -> 空（Go: len==0 -> c.Next()）。
        let candidates = extract_client_ip_candidates(None, &[]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn extract_client_ip_dedupes_repeated_values_preserving_order() {
        // Go seen map 去重，保序——重复的 connect_ip 与 xff 第一跳相同不重复入列。
        // 注意：Go 只取 XFF 第一跳（strings.Cut(xff, ",")），第二跳 8.8.8.8 不会被考虑。
        let headers = vec![
            (
                "X-Forwarded-For".to_string(),
                "127.0.0.1, 8.8.8.8".to_string(),
            ),
            ("X-Real-IP".to_string(), "127.0.0.1".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1"]);
    }

    #[test]
    fn extract_client_ip_trims_whitespace_like_go_add() {
        // Go add() 对每个值 TrimSpace；header value 带前后空格也该被清理。
        let headers = vec![
            (
                "X-Forwarded-For".to_string(),
                "  203.0.113.5  , 1.2.3.4".to_string(),
            ),
            ("X-Real-IP".to_string(), "\t10.0.0.1\t".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some(" 127.0.0.1 "), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "203.0.113.5", "10.0.0.1"]);
    }

    #[test]
    fn extract_client_ip_empty_xff_value_skipped_falls_through_to_x_real_ip() {
        // XFF 值为空串（或纯空白）应被跳过，不影响后续 X-Real-IP 入列。
        let headers = vec![
            ("X-Forwarded-For".to_string(), "   ".to_string()),
            ("X-Real-IP".to_string(), "192.0.2.10".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "192.0.2.10"]);
    }

    #[test]
    fn extract_client_ip_header_name_case_insensitive() {
        // HTTP header 名大小写不敏感；传入小写也该命中。
        let headers = vec![
            ("x-forwarded-for".to_string(), "203.0.113.5".to_string()),
            ("X-REAL-IP".to_string(), "10.0.0.7".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "203.0.113.5", "10.0.0.7"]);
    }

    #[test]
    fn extract_client_ip_dedupes_when_x_real_ip_equals_connect_ip() {
        // 去重跨源生效：connect_ip == X-Real-IP 时，X-Real-IP 不再次入列。
        let headers = vec![
            ("X-Forwarded-For".to_string(), "203.0.113.5".to_string()),
            ("X-Real-IP".to_string(), "127.0.0.1".to_string()),
        ];

        let candidates = extract_client_ip_candidates(Some("127.0.0.1"), &headers);

        assert_eq!(candidates, vec!["127.0.0.1", "203.0.113.5"]);
    }

    // ===================================================================
    // RUST-P11-001 S17 — RequestContext 注入中间件 (Zeno-the-3rd)
    // 镜像 Go auth.go (WithJWTAuth L74-110, WithAPIKeyConfig L27-72) +
    // project.go L14-33 + contexts.FromContext。
    // ===================================================================

    // ---- ① 中间件注入 RequestContext 后 handler 可见 ----

    #[test]
    fn insert_auth_request_context_round_trips_through_extensions() {
        // 镜像 Go `c.Request = c.Request.WithContext(ctx)` +
        // `contexts.FromContext(ctx)`。强类型 Extension 索引保证 handler 能取回。
        let ctx = build_context_from_api_key_auth(
            conduit_auth::Principal::api_key("key-1", "project-7"),
            Some("7".to_string()),
            conduit_auth::RequestSource::OpenAi,
            Some("req-1".to_string()),
            Some("127.0.0.1".to_string()),
        );
        let extension = AuthRequestContextExtension::new(ctx);

        let mut request: Request<Body> = Request::new(Body::empty());
        assert!(insert_auth_request_context(&mut request, extension).is_none());
        let extracted = extract_auth_request_context(&request);
        assert!(extracted.is_some(), "auth context must reach downstream");

        let ctx_ref = extracted.map(|extension| extension.context()).map(|ctx| {
            (
                ctx.principal.as_ref().map(|p| p.to_string()),
                ctx.project_id.clone(),
            )
        });
        assert_eq!(
            ctx_ref,
            Some((Some("apikey:key-1".to_string()), Some("7".to_string())))
        );
    }

    #[tokio::test]
    async fn s17_middleware_injected_context_is_visible_to_handler() -> Result<(), Box<dyn Error>> {
        // 端到端:middleware 注入 -> handler 通过 axum::Extension 取出,不查 API key。
        async fn handler(Extension(auth): Extension<AuthRequestContextExtension>) -> String {
            let ctx = auth.context();
            format!(
                "principal={} project={:?} source={}",
                ctx.principal
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                ctx.project_id,
                ctx.source
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            )
        }

        let mut app = Router::new().route("/v1/x", get(handler));
        let ctx = build_context_from_api_key_auth(
            conduit_auth::Principal::api_key_service_account("sa-1", "9"),
            Some("9".to_string()),
            conduit_auth::RequestSource::AdminRest,
            None,
            None,
        );
        let mut request = Request::builder().uri("/v1/x").body(Body::empty())?;
        insert_auth_request_context(&mut request, AuthRequestContextExtension::new(ctx));

        let response = app.call(request).await?;
        let body = to_bytes(response.into_body(), 256).await?;
        let body = std::str::from_utf8(&body)?;
        assert!(
            body.contains("apikey:sa-1"),
            "principal reaches handler: {body}"
        );
        assert!(
            body.contains("project=Some(\"9\")"),
            "project reaches handler: {body}"
        );
        assert!(
            body.contains("source=admin_rest"),
            "source reaches handler: {body}"
        );
        Ok(())
    }

    // ---- ② 无 auth -> 401/403 ----

    #[test]
    fn s17_missing_auth_extension_signals_handler_to_reject() {
        // 没有 auth middleware 运行时,extension 不存在。handler 据此返回 401,
        // 而非自行调用 extract_api_key 兜底。镜像 Go:WithJWTAuth 失败时
        // `AbortWithError(c, 401, ...)` 阻止 c.Next()。
        let request: Request<Body> = Request::new(Body::empty());
        assert!(extract_auth_request_context(&request).is_none());
    }

    #[test]
    fn s17_jwt_invalid_token_returns_invalid_outcome_with_401_message() {
        // 镜像 auth.go L87-88:ErrInvalidJWT -> 401 "Invalid token"。
        let outcome = verify_jwt_and_build_context(
            "not-a-real-token",
            b"secret",
            conduit_auth::RequestSource::AdminRest,
            None,
            None,
        );
        let Err(err) = outcome else {
            panic!("invalid token must error, not authenticate");
        };
        let auth_outcome = jwt_auth_outcome(Err(err));
        assert_eq!(auth_outcome.status, JwtAuthStatus::Invalid);
        assert_eq!(
            auth_outcome.public_message,
            Some(JWT_INVALID_PUBLIC_MESSAGE)
        );
    }

    #[test]
    fn s17_api_key_auth_outcome_invalid_returns_401_message() {
        // 镜像 auth.go L44:NotFound/ErrInvalidAPIKey -> 401 "Invalid API key"。
        let outcome = api_key_auth_outcome(Err(ApiKeyAuthError::Invalid));
        assert_eq!(outcome.status, ApiKeyAuthStatus::Invalid);
        assert_eq!(outcome.public_message, Some(API_KEY_INVALID_PUBLIC_MESSAGE));
    }

    #[test]
    fn s17_openapi_regular_key_forbidden_returns_403() {
        // 镜像 auth.go L140-143 + S11:非 service_account -> 403。
        let (status, msg) = open_api_auth_outcome(Ok(ApiKeyKind::Regular));
        assert_eq!(status, OpenApiAuthStatus::Forbidden);
        assert_eq!(msg, Some("service account API key required"));
    }

    // ---- ③ project scoping 生效(project_id 从 header 解析注入)----

    #[test]
    fn s17_apply_project_id_header_populates_context_like_go_project_go()
    -> Result<(), Box<dyn Error>> {
        // 镜像 project.go L22-28:合法 `gid://conduit/Project/<n>` -> WithProjectID。
        let mut ctx =
            build_context_from_jwt(42, conduit_auth::RequestSource::AdminRest, None, None);
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/Project/77"),
        );

        apply_project_id_header(&mut ctx, &headers)?;

        assert_eq!(ctx.project_id.as_deref(), Some("77"));
        Ok(())
    }

    #[test]
    fn s17_apply_project_id_header_missing_leaves_context_untouched_like_go_next()
    -> Result<(), Box<dyn Error>> {
        // 镜像 project.go L17-20:header 缺失 -> c.Next(),不报错也不覆盖现有值。
        let mut ctx = build_context_from_jwt(1, conduit_auth::RequestSource::AdminRest, None, None);
        apply_project_id_header(&mut ctx, &HeaderMap::new())?;
        assert!(
            ctx.project_id.is_none(),
            "missing header must not set project_id"
        );
        Ok(())
    }

    #[test]
    fn s17_apply_project_id_header_malformed_returns_400_like_go() {
        // 镜像 project.go L23-26:parseErr != nil || type != Project -> 400。
        let mut ctx = build_context_from_jwt(1, conduit_auth::RequestSource::AdminRest, None, None);
        let mut headers = HeaderMap::new();
        headers.insert(PROJECT_ID_HEADER, HeaderValue::from_static("not-a-guid"));

        let Err(err) = apply_project_id_header(&mut ctx, &headers) else {
            panic!("malformed project header must return 400");
        };
        assert_eq!(err.http_status, 400);
        assert_eq!(err.public_message(), "Invalid project ID");
        // 关键:malformed header 不得污染 ctx。
        assert!(ctx.project_id.is_none());
    }

    #[test]
    fn s17_apply_project_id_header_wrong_entity_type_returns_400() {
        let mut ctx = build_context_from_jwt(1, conduit_auth::RequestSource::AdminRest, None, None);
        let mut headers = HeaderMap::new();
        headers.insert(
            PROJECT_ID_HEADER,
            HeaderValue::from_static("gid://conduit/User/5"),
        );

        let Err(err) = apply_project_id_header(&mut ctx, &headers) else {
            panic!("wrong entity type must 400");
        };
        assert_eq!(err.http_status, 400);
    }

    // ---- ④ handler 不自查 api_key(守卫测试)----

    #[test]
    fn s17_handler_extracted_context_does_not_carry_raw_api_key_string()
    -> Result<(), Box<dyn Error>> {
        // 守卫:principal 只携带标识符 (apikey:key-1),不携带原始 Authorization
        // header 的 token 字符串。这强制 handler 只能通过 `RequestContext`
        // 读取鉴权结果,无法在 ctx 中拿到原始 key 字符串去自查。
        let principal = conduit_auth::Principal::api_key("resolved-key-id", "project-1");
        let ctx = build_context_from_api_key_auth(
            principal,
            Some("1".to_string()),
            conduit_auth::RequestSource::OpenAi,
            Some("req-9".to_string()),
            None,
        );

        // 序列化后的 RequestContext 绝不能包含原始 Bearer token 字符串。
        let serialized = serde_json::to_string(&ctx)?;
        assert!(
            !serialized.contains("sk-secret"),
            "raw secret must NOT appear in RequestContext: {serialized}"
        );
        // 但 resolved key id 会出现(非密)。
        assert!(serialized.contains("resolved-key-id"));
        Ok(())
    }

    #[test]
    fn s17_request_context_safe_summary_does_not_leak_secrets() {
        // 与 Display/Debug 一致:safe_summary 用于日志,绝不能含密钥。
        let principal = conduit_auth::Principal::api_key("key-id-1", "project-1");
        let ctx = build_context_from_api_key_auth(
            principal,
            Some("1".to_string()),
            conduit_auth::RequestSource::OpenAi,
            None,
            None,
        );
        let summary = ctx.safe_summary();
        assert!(summary.contains("apikey:key-id-1"));
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("Bearer"));
    }

    // ---- ⑤ JWT 中间件端到端(verify token → Claims → Principal → RequestContext)----

    #[test]
    fn s17_jwt_round_trip_builds_context_with_user_principal_and_scope()
    -> Result<(), Box<dyn Error>> {
        // 镜像 auth.go L96-106:AuthenticateJWTToken 成功后注入 user + principal +
        // session_scope "user:<id>"。
        use conduit_auth::jwt::{Claims, encode_hs256};

        let claims = Claims::new(42, "user:42".to_string());
        let token = encode_hs256(&claims, b"secret")?;

        let ctx = verify_jwt_and_build_context(
            &token,
            b"secret",
            conduit_auth::RequestSource::AdminRest,
            Some("req-1".to_string()),
            None,
        )
        .map_err(|_| "jwt verify must succeed for round-trip token")?;

        let principal = ctx
            .principal
            .as_ref()
            .ok_or("principal must be set after jwt auth")?;
        assert_eq!(principal.kind, conduit_auth::PrincipalKind::User);
        assert_eq!(principal.id.as_deref(), Some("42"));
        assert_eq!(
            principal.session_scope.as_deref(),
            Some("user:42"),
            "session_scope 必须匹配 Go shared.WithSessionScope 格式"
        );
        assert_eq!(ctx.user.as_ref().map(|u| u.user_id), Some(42));
        Ok(())
    }

    #[test]
    fn s17_jwt_expired_token_is_rejected_as_invalid() -> Result<(), Box<dyn Error>> {
        // 镜像 auth.go L87-88:过期 token 属于 ErrInvalidJWT -> 401。
        use conduit_auth::jwt::{Claims, encode_hs256};

        // 直接用 i64 时间戳(秒)避免引入 chrono 依赖。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let expired = Claims {
            user_id: 1,
            session_scope: "user:1".into(),
            exp: now - 86_400, // 1 天前过期
            iat: now - 2 * 86_400,
        };
        let token = encode_hs256(&expired, b"secret")?;

        let result = verify_jwt_and_build_context(
            &token,
            b"secret",
            conduit_auth::RequestSource::AdminRest,
            None,
            None,
        );
        let Err(err) = result else {
            panic!("expired token must not authenticate");
        };
        assert_eq!(err, JwtAuthError::Invalid);
        let outcome = jwt_auth_outcome(Err(err));
        assert_eq!(outcome.status, JwtAuthStatus::Invalid);
        Ok(())
    }

    #[test]
    fn s17_jwt_wrong_secret_is_rejected_as_invalid_not_internal() -> Result<(), Box<dyn Error>> {
        // 关键:签名不匹配属于客户端错误(Invalid -> 401),而非服务端故障(500)。
        // 不能把 jwt 内部细节透给客户端。
        use conduit_auth::jwt::{Claims, encode_hs256};

        let claims = Claims::new(7, "user:7".to_string());
        let token = encode_hs256(&claims, b"real-secret")?;

        let result = verify_jwt_and_build_context(
            &token,
            b"wrong-secret",
            conduit_auth::RequestSource::AdminRest,
            None,
            None,
        );
        let Err(err) = result else {
            panic!("wrong-secret token must not authenticate");
        };
        assert_eq!(err, JwtAuthError::Invalid);
        // 公开消息不能包含 jwt 内部原因。
        let outcome = jwt_auth_outcome(Err(err));
        let msg = outcome.public_message.unwrap_or("");
        assert!(!msg.contains("signature"));
        assert!(!msg.contains("jwt"));
        Ok(())
    }
}

//! GAP-P0 — dashboard first-paint blocking queries (`me` slice).
//!
//! Ports the three GraphQL operations the admin frontend calls synchronously
//! during its three-layer bootstrap guard. Without them no page renders. The
//! Go contract lives in:
//!   - `conduit/internal/server/gql/me.graphql` (`me`, `myProjects`)
//!   - `conduit/internal/server/gql/system.graphql` (`systemStatus`)
//!
//! and the captured snapshot at `tests/contracts/admin_graphql_schema.graphql`.
//!
//! ## Operations ported (3 queries)
//!
//!   - `Query.me: UserInfo!` — Go resolver `Me`
//!     (`me.resolvers.go:112-127`): read the current user from request
//!     context, load the full user (with preloaded edges) and convert it to
//!     `objects.UserInfo` via `biz.ConvertUserToUserInfo`.
//!   - `Query.myProjects: [Project!]!` — Go resolver `MyProjects`
//!     (`me.resolvers.go:130-143`): read the current user, then return the
//!     active projects the user belongs to.
//!   - `Query.systemStatus: SystemStatus!` — Go resolver `SystemStatus`
//!     (`system.resolvers.go:372-381`): delegate to
//!     `systemService.IsInitialized` (`biz/system.go:630`).
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB / HTTP / request-context concerns.
//! The host wires concrete implementations of [`MeServices`] and
//! [`SystemStatusServices`] into the schema data bag. The host implementation
//! is responsible for resolving the current user from the request-scoped
//! context (the Go resolvers read it from `contexts.GetUser(ctx)`); the trait
//! surface here therefore takes no explicit principal argument, mirroring the
//! dependency-injection pattern already used by
//! [`crate::system::SystemSettingsServices`].
//!
//! Resolver-level tests inject in-memory fakes and cover both the wired path
//! and the unwired "service unavailable" fallback.

use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

use crate::project::Project;

// ===========================================================================
// Output types — GraphQL mirrors of the Go `objects.*` / gql types.
// ===========================================================================

/// GraphQL `RoleInfo` (snapshot lines 8858-8860). Mirrors Go
/// `objects.RoleInfo` (`internal/objects/user.go:33-35`): a single non-null
/// `name` field.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "RoleInfo")]
pub struct RoleInfo {
    pub name: String,
}

/// GraphQL `OIDCIdentityInfo` (snapshot lines 8850-8856). Mirrors Go
/// `objects.OIDCIdentityInfo` (`internal/objects/user.go:18-24`).
///
/// The type name carries the all-caps `OIDC` acronym — an explicit
/// `#[graphql(name = ...)]` is required, otherwise async-graphql would derive
/// `OidcIdentityInfo` from the Rust identifier.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "OIDCIdentityInfo")]
pub struct OidcIdentityInfo {
    /// `id: ID!` — GUID wire form `gid://conduit/OIDCIdentity/<id>` (Go
    /// `objects.GUID{Type: ent.TypeOIDCIdentity, ID: identity.ID}`).
    pub id: ID,
    pub idp_name: String,
    pub issuer: String,
    pub subject: String,
    pub email: String,
}

/// GraphQL `UserProjectInfo` (snapshot lines 8862-8867). Mirrors Go
/// `objects.UserProjectInfo` (`internal/objects/user.go:26-31`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "UserProjectInfo")]
pub struct UserProjectInfo {
    /// `projectID: ID!` — all-caps `ID` acronym tag; camelCase would emit
    /// `projectId` (Go json tag is `projectID`, snapshot line 8863). Carries
    /// the GUID wire form `gid://conduit/Project/<id>`.
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub is_owner: bool,
    pub scopes: Vec<String>,
    pub roles: Vec<RoleInfo>,
}

/// GraphQL `UserInfo` (snapshot lines 8835-8848). Mirrors Go
/// `objects.UserInfo` (`internal/objects/user.go:3-16`), produced by
/// `biz.ConvertUserToUserInfo` (`biz/user.go:279-364`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "UserInfo")]
pub struct UserInfo {
    /// `id: ID!` — GUID wire form `gid://conduit/User/<id>`.
    pub id: ID,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub is_owner: bool,
    pub prefer_language: String,
    /// `avatar: String` — nullable in the contract (Go `*string`).
    pub avatar: Option<String>,
    pub scopes: Vec<String>,
    pub roles: Vec<RoleInfo>,
    pub projects: Vec<UserProjectInfo>,
    /// `oidcIdentities: [OIDCIdentityInfo!]!` — camelCase of
    /// `oidc_identities` renders `oidcIdentities`, matching the contract
    /// (snapshot line 8846); no explicit rename needed.
    pub oidc_identities: Vec<OidcIdentityInfo>,
    pub has_password: bool,
}

/// GraphQL `SystemStatus` (snapshot lines 9357-9359). Mirrors the Go gql type
/// resolved by `SystemStatus` (`system.resolvers.go:372-381`): a single
/// non-null `isInitialized` flag from `systemService.IsInitialized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "SystemStatus")]
pub struct SystemStatus {
    pub is_initialized: bool,
}

// ===========================================================================
// Service traits (host-injected)
// ===========================================================================

/// Error surface for the `me` slice. Messages mirror the Go
/// `fmt.Errorf("...: %w")` prefixes so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum MeError {
    /// No service wired into the schema data bag (e.g. the bare SDL-smoke
    /// schema). Surfaced instead of panicking.
    #[error("me service is not available")]
    ServiceUnavailable,
    /// Mirrors Go `me.resolvers.go:116` / `:134`
    /// (`fmt.Errorf("user not found in context")`).
    #[error("user not found in context")]
    UserNotFound,
    /// Mirrors Go `me.resolvers.go:122`
    /// (`fmt.Errorf("failed to get user details: %w", err)`).
    #[error("failed to get user details: {0}")]
    UserDetails(String),
    /// Mirrors Go `me.resolvers.go:130-143` — the project query failure.
    #[error("failed to get my projects: {0}")]
    MyProjects(String),
}

/// Error surface for the `systemStatus` query.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SystemStatusError {
    #[error("system service is not available")]
    ServiceUnavailable,
    /// Mirrors Go `system.resolvers.go:375`
    /// (`fmt.Errorf("failed to check initialization status: %w", err)`).
    #[error("failed to check initialization status: {0}")]
    InitializationStatus(String),
}

/// Per-request identity of the authenticated caller, injected into the
/// async-graphql request data bag by the host's GraphQL handler.
///
/// The admin schema is built once at boot (a singleton), so a wired
/// [`MeServices`] adapter cannot know *which* user issued a given request.
/// Go reads the current user from the gin request context
/// (`contexts.GetUser(ctx)`); the Rust port mirrors this by pulling the
/// `user_id` off the JWT auth middleware's request extension and pushing it
/// into the per-request async-graphql data bag (see the host `graphql_handler`).
/// The resolver then reads it via [`current_user`] and passes it to the
/// service — matching Go's `userService.GetUserByID(ctx, userCtx.ID)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentUser {
    /// The authenticated user's numeric id (Go `user.ID`).
    pub user_id: i64,
}

/// Backs `Query.me` and `Query.myProjects`. Both Go resolvers read the current
/// user from request context (`contexts.GetUser`) then load by id. The trait
/// takes the resolved `user_id` explicitly: the host GraphQL handler resolves
/// it per-request from the JWT auth extension and the resolver forwards it,
/// mirroring Go's `GetUserByID(ctx, userCtx.ID)` / project query on `u.ID`.
#[async_trait::async_trait]
pub trait MeServices: Send + Sync {
    /// Mirrors Go resolver `Me` (`me.resolvers.go:112-127`): load the full
    /// user identified by `user_id` and convert it to [`UserInfo`].
    async fn me(&self, user_id: i64) -> Result<UserInfo, MeError>;

    /// Mirrors Go resolver `MyProjects` (`me.resolvers.go:130-143`): return
    /// the active projects `user_id` belongs to.
    async fn my_projects(&self, user_id: i64) -> Result<Vec<Project>, MeError>;
}

/// Backs `Query.systemStatus`. Mirrors the Go `systemService.IsInitialized`
/// call in the `SystemStatus` resolver.
#[async_trait::async_trait]
pub trait SystemStatusServices: Send + Sync {
    /// Mirrors Go resolver `SystemStatus` (`system.resolvers.go:372-381`):
    /// return `{ isInitialized }` from `systemService.IsInitialized`.
    async fn system_status(&self) -> Result<SystemStatus, SystemStatusError>;
}

// ===========================================================================
// Context helpers.
// ===========================================================================

/// Resolves the injected [`MeServices`] from the async-graphql context data
/// bag, surfacing the Go-equivalent "service unavailable" message when no
/// service was wired.
pub(crate) fn me_services(ctx: &Context<'_>) -> Result<Arc<dyn MeServices>, String> {
    match ctx.data::<Arc<dyn MeServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(MeError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the per-request [`CurrentUser`] from the async-graphql data bag,
/// surfacing the Go "user not found in context" message when the host handler
/// did not inject one (unauthenticated request / missing JWT extension).
/// Mirrors Go `me.resolvers.go:114-117` where `!ok || userCtx == nil` yields
/// `fmt.Errorf("user not found in context")`.
pub(crate) fn current_user(ctx: &Context<'_>) -> Result<CurrentUser, String> {
    match ctx.data_opt::<CurrentUser>() {
        Some(user) => Ok(*user),
        None => Err(MeError::UserNotFound.to_string()),
    }
}

/// Resolves the injected [`SystemStatusServices`] from the async-graphql
/// context data bag.
pub(crate) fn system_status_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn SystemStatusServices>, String> {
    match ctx.data::<Arc<dyn SystemStatusServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(SystemStatusError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring — Query methods live on the single `#[Object] impl
// QueryRoot` in lib.rs (async-graphql forbids splitting it across modules).
// This slice exposes the typed service-lookup helpers above; lib.rs delegates
// `me` / `myProjects` / `systemStatus` to them.
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use async_graphql::{EmptySubscription, Name, Schema, Value};

    use super::*;
    use crate::mutation::MutationRoot;
    use crate::project::ProjectStatus;
    use crate::scalars::TimeScalar;

    // ---------------------------------------------------------------------
    // In-memory fakes.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeMeServices {
        user: Option<UserInfo>,
        me_error: Option<MeError>,
        projects: Vec<Project>,
        projects_error: Option<MeError>,
    }

    #[async_trait::async_trait]
    impl MeServices for FakeMeServices {
        async fn me(&self, _user_id: i64) -> Result<UserInfo, MeError> {
            if let Some(err) = &self.me_error {
                return Err(err.clone());
            }
            match &self.user {
                Some(u) => Ok(u.clone()),
                None => Err(MeError::UserNotFound),
            }
        }

        async fn my_projects(&self, _user_id: i64) -> Result<Vec<Project>, MeError> {
            match &self.projects_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.projects.clone()),
            }
        }
    }

    #[derive(Default, Clone)]
    struct FakeSystemStatusServices {
        is_initialized: bool,
        error: Option<SystemStatusError>,
    }

    #[async_trait::async_trait]
    impl SystemStatusServices for FakeSystemStatusServices {
        async fn system_status(&self) -> Result<SystemStatus, SystemStatusError> {
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(SystemStatus {
                    is_initialized: self.is_initialized,
                }),
            }
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with_me(services: FakeMeServices) -> TestSchema {
        let arc: Arc<dyn MeServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    /// Build a request carrying a per-request [`CurrentUser`], mirroring what
    /// the host `graphql_handler` injects from the JWT auth extension.
    fn request_as_user(query: &str, user_id: i64) -> async_graphql::Request {
        async_graphql::Request::new(query).data(CurrentUser { user_id })
    }

    fn schema_with_system_status(services: FakeSystemStatusServices) -> TestSchema {
        let arc: Arc<dyn SystemStatusServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    fn sample_user() -> UserInfo {
        UserInfo {
            id: ID::from("gid://conduit/User/1"),
            email: "ada@example.com".to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            is_owner: true,
            prefer_language: "en".to_string(),
            avatar: Some("https://x/y.png".to_string()),
            scopes: vec!["read".to_string()],
            roles: vec![RoleInfo {
                name: "admin".to_string(),
            }],
            projects: vec![UserProjectInfo {
                project_id: ID::from("gid://conduit/Project/1"),
                is_owner: true,
                scopes: vec!["read".to_string()],
                roles: vec![RoleInfo {
                    name: "owner".to_string(),
                }],
            }],
            oidc_identities: vec![OidcIdentityInfo {
                id: ID::from("gid://conduit/OIDCIdentity/1"),
                idp_name: "google".to_string(),
                issuer: "https://g".to_string(),
                subject: "sub".to_string(),
                email: "ada@example.com".to_string(),
            }],
            has_password: true,
        }
    }

    fn sample_project() -> Project {
        Project {
            id: ID::from("gid://conduit/Project/1"),
            created_at: TimeScalar(chrono::Utc::now()),
            updated_at: TimeScalar(chrono::Utc::now()),
            name: "P1".to_string(),
            description: "desc".to_string(),
            status: ProjectStatus::Active,
            profiles: None,
        }
    }

    // ---- resolver: me -----------------------------------------------

    #[tokio::test]
    async fn me_returns_user_info_fields() {
        let fake = FakeMeServices {
            user: Some(sample_user()),
            ..FakeMeServices::default()
        };
        let schema = schema_with_me(fake);

        let resp = schema
            .execute(request_as_user(
                "{ me { id email firstName lastName isOwner preferLanguage avatar scopes roles { name } projects { projectID isOwner scopes roles { name } } oidcIdentities { id idpName issuer subject email } hasPassword } }",
                1,
            ))
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let me = match obj.get(&Name::new("me")) {
            Some(v) => v,
            None => panic!("me field missing in {obj:?}"),
        };
        let fields = as_object(me);
        match fields.get(&Name::new("email")) {
            Some(Value::String(s)) => assert_eq!(s, "ada@example.com"),
            other => panic!("email field unexpected: {other:?}"),
        }
        // Acronym renames present verbatim.
        let rendered = resp.data.to_string();
        assert!(
            rendered.contains("projectID:"),
            "projectID missing: {rendered}"
        );
        assert!(
            rendered.contains("oidcIdentities:"),
            "oidcIdentities missing: {rendered}"
        );
    }

    #[tokio::test]
    async fn me_surfaces_user_not_found() {
        // Go resolver (me.resolvers.go:116): no user in context -> error.
        let fake = FakeMeServices::default();
        let schema = schema_with_me(fake);

        let resp = schema.execute("{ me { id } }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("user not found in context"), "msg: {msg}");
    }

    #[tokio::test]
    async fn me_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();

        let resp = schema.execute("{ me { id } }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("me service is not available"), "msg: {msg}");
    }

    // ---- resolver: my_projects --------------------------------------

    #[tokio::test]
    async fn my_projects_returns_list() {
        let fake = FakeMeServices {
            user: Some(sample_user()),
            projects: vec![sample_project()],
            ..FakeMeServices::default()
        };
        let schema = schema_with_me(fake);

        let resp = schema
            .execute(request_as_user(
                "{ myProjects { id name description status } }",
                1,
            ))
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("myProjects")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 1),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn my_projects_surfaces_error() {
        let fake = FakeMeServices {
            user: Some(sample_user()),
            projects_error: Some(MeError::MyProjects("db down".to_string())),
            ..FakeMeServices::default()
        };
        let schema = schema_with_me(fake);

        let resp = schema
            .execute(request_as_user("{ myProjects { id } }", 1))
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to get my projects"), "msg: {msg}");
        assert!(msg.contains("db down"), "msg: {msg}");
    }

    #[tokio::test]
    async fn my_projects_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();

        let resp = schema.execute("{ myProjects { id } }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("me service is not available"), "msg: {msg}");
    }

    // ---- resolver: system_status ------------------------------------

    #[tokio::test]
    async fn system_status_returns_is_initialized() {
        let fake = FakeSystemStatusServices {
            is_initialized: true,
            error: None,
        };
        let schema = schema_with_system_status(fake);

        let resp = schema.execute("{ systemStatus { isInitialized } }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let status = match obj.get(&Name::new("systemStatus")) {
            Some(v) => v,
            None => panic!("systemStatus field missing"),
        };
        let fields = as_object(status);
        match fields.get(&Name::new("isInitialized")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("isInitialized unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn system_status_surfaces_error() {
        // Go resolver (system.resolvers.go:375): IsInitialized failure.
        let fake = FakeSystemStatusServices {
            is_initialized: false,
            error: Some(SystemStatusError::InitializationStatus("boom".to_string())),
        };
        let schema = schema_with_system_status(fake);

        let resp = schema.execute("{ systemStatus { isInitialized } }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to check initialization status"),
            "msg: {msg}"
        );
        assert!(msg.contains("boom"), "msg: {msg}");
    }

    #[tokio::test]
    async fn system_status_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();

        let resp = schema.execute("{ systemStatus { isInitialized } }").await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("system service is not available"),
            "msg: {msg}"
        );
    }

    // ---- SDL shape parity -------------------------------------------

    #[test]
    fn sdl_contains_me_slice_types_and_signatures() {
        let me_arc: Arc<dyn MeServices> = Arc::new(FakeMeServices::default());
        let status_arc: Arc<dyn SystemStatusServices> =
            Arc::new(FakeSystemStatusServices::default());
        let sdl = crate::admin_schema_builder()
            .data(me_arc)
            .data(status_arc)
            .finish()
            .sdl();

        for expected in [
            "type UserInfo {",
            "type RoleInfo {",
            "type OIDCIdentityInfo {",
            "type UserProjectInfo {",
            "type SystemStatus {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        for expected in [
            "me: UserInfo!",
            "myProjects: [Project!]!",
            "systemStatus: SystemStatus!",
            "isInitialized: Boolean!",
            "hasPassword: Boolean!",
            "oidcIdentities: [OIDCIdentityInfo!]!",
            "projectID: ID!",
            "isOwner: Boolean!",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }
    }
}

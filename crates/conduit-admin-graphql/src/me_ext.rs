//! GAP-H — `me` account-domain mutations.
//!
//! Ports the three self-service account mutations the admin frontend
//! settings/profile pages call. Every input type is copied field-for-field
//! from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`, and every resolver mirrors
//! the Go body in `conduit/internal/server/gql/me.resolvers.go`.
//!
//! ## Mutations ported (snapshot `type Mutation` lines 8882-8884)
//!
//!   - `updateMe(input: UpdateMeInput!): User!` (snapshot 8882) — Go
//!     `UpdateMe` (`me.resolvers.go`): read the current user from request
//!     context, then `UserService.UpdateUser(ctx, userCtx.ID,
//!     biz.UpdateUserParams{FirstName, LastName, PreferLanguage, Avatar})`.
//!     Partial merge — every input field is optional.
//!   - `updateMyPassword(input: UpdateMyPasswordInput!): Boolean!` (snapshot
//!     8883) — Go `UpdateMyPassword`: `UserService.UpdatePassword(ctx,
//!     userCtx.ID, oldPassword, newPassword)`.
//!   - `unlinkOIDCIdentity(id: ID!): Boolean!` (snapshot 8884) — Go
//!     `UnlinkOIDCIdentity`: ownership check + last-identity guard, then
//!     delete `OIDCIdentity` by id (scoped to the current user).
//!
//! ## Return type
//!
//! `updateMe` returns `User!` (snapshot 8882 — NOT `UserInfo!`; that is the
//! `me` *query* return). We reuse [`crate::user::User`].
//!
//! ## Current-user resolution
//!
//! All three Go resolvers read `contexts.GetUser(ctx)` and fail with
//! `"user not found in context"` when absent. The Rust port reuses the
//! per-request [`crate::me::CurrentUser`] injected by the host GraphQL handler
//! (from the JWT auth extension) and the [`crate::me::current_user`] helper —
//! the same mechanism the `me` / `myProjects` queries use. The host trait
//! methods therefore take the resolved `user_id` explicitly, mirroring Go's
//! `UserService.UpdateUser(ctx, userCtx.ID, …)`.
//!
//! ## Service wiring — MutationRoot delegates (async-graphql forbids a second
//! `#[Object] impl MutationRoot`)
//!
//! The three resolver bodies live in the single `#[Object] impl MutationRoot`
//! block in `crate::mutation`; each reads the [`MeMutationServices`] service +
//! the [`crate::me::CurrentUser`] from the data bag and delegates here. The
//! canonical bodies (pasted into `mutation.rs`):
//!
//! ```ignore
//! /// `Mutation.updateMe(input: UpdateMeInput!): User!` — Go `me.resolvers.go`
//! /// `UpdateMe`.
//! async fn update_me(
//!     &self,
//!     ctx: &Context<'_>,
//!     input: crate::me_ext::UpdateMeInput,
//! ) -> Result<crate::user::User, String> {
//!     let user_id = crate::me::current_user(ctx)?.user_id;
//!     let services = crate::me_ext::me_mutation_services(ctx)?;
//!     services.update_me(user_id, input).await.map_err(|e| e.to_string())
//! }
//!
//! /// `Mutation.updateMyPassword(input: UpdateMyPasswordInput!): Boolean!` —
//! /// Go `me.resolvers.go` `UpdateMyPassword`.
//! async fn update_my_password(
//!     &self,
//!     ctx: &Context<'_>,
//!     input: crate::me_ext::UpdateMyPasswordInput,
//! ) -> Result<bool, String> {
//!     let user_id = crate::me::current_user(ctx)?.user_id;
//!     let services = crate::me_ext::me_mutation_services(ctx)?;
//!     services
//!         .update_my_password(user_id, input.old_password, input.new_password)
//!         .await
//!         .map_err(|e| e.to_string())?;
//!     Ok(true)
//! }
//!
//! /// `Mutation.unlinkOIDCIdentity(id: ID!): Boolean!` — Go `me.resolvers.go`
//! /// `UnlinkOIDCIdentity`.
//! #[graphql(name = "unlinkOIDCIdentity")]
//! async fn unlink_oidc_identity(
//!     &self,
//!     ctx: &Context<'_>,
//!     id: async_graphql::ID,
//! ) -> Result<bool, String> {
//!     let user_id = crate::me::current_user(ctx)?.user_id;
//!     let services = crate::me_ext::me_mutation_services(ctx)?;
//!     services
//!         .unlink_oidc_identity(user_id, id.to_string())
//!         .await
//!         .map_err(|e| e.to_string())?;
//!     Ok(true)
//! }
//! ```
//!
//! Resolver-level tests inject an in-memory fake and cover the wired path plus
//! the unwired "me service is not available" and "user not found in context"
//! fallbacks.

use std::sync::Arc;

use async_graphql::{Context, InputObject};

use crate::user::User;

// ===========================================================================
// Input types — GraphQL mirrors of the Go `gmodel.*` input structs.
// ===========================================================================

/// `input UpdateMeInput` — snapshot lines 8874-8879. All four fields are
/// optional (partial merge). Field names are hand-written camelCase in the Go
/// schema (`firstName`/`lastName`/`preferLanguage`/`avatar`), which the
/// default async-graphql renaming matches — no explicit `#[graphql(name)]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateMeInput")]
pub struct UpdateMeInput {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub prefer_language: Option<String>,
    pub avatar: Option<String>,
}

/// `input UpdateMyPasswordInput` — snapshot lines 8887-8890. `oldPassword` is
/// nullable in the contract (`String`), `newPassword` is non-null (`String!`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateMyPasswordInput")]
pub struct UpdateMyPasswordInput {
    pub old_password: Option<String>,
    pub new_password: String,
}

// ===========================================================================
// Service trait (host-injected)
// ===========================================================================

/// Error surface for the `me` mutation slice. Messages mirror the Go
/// `fmt.Errorf("...: %w")` prefixes in `me.resolvers.go` so frontend error
/// handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum MeMutationError {
    /// No service wired into the schema data bag (e.g. the bare SDL-smoke
    /// schema). Surfaced instead of panicking.
    #[error("me service is not available")]
    ServiceUnavailable,
    /// Mirrors Go `me.resolvers.go` `UpdateMe`
    /// (`fmt.Errorf("failed to update user: %w", err)`).
    #[error("failed to update user: {0}")]
    UpdateUser(String),
    /// Mirrors Go `me.resolvers.go` `UpdateMyPassword`
    /// (`fmt.Errorf("failed to update password: %w", err)`).
    #[error("failed to update password: {0}")]
    UpdatePassword(String),
    /// Mirrors Go `me.resolvers.go` `UnlinkOIDCIdentity`
    /// (`fmt.Errorf("failed to unlink identity: %w", err)`). The Go resolver
    /// also emits ownership / last-identity guard errors; the host surfaces
    /// those through this variant's message (see the trait doc).
    #[error("failed to unlink identity: {0}")]
    UnlinkIdentity(String),
}

/// Backs the three `me` account mutations. Each Go resolver reads the current
/// user from request context (`contexts.GetUser`) then calls `UserService`
/// with `userCtx.ID`; the trait takes the resolved `user_id` explicitly (the
/// resolver reads it from the per-request [`crate::me::CurrentUser`] and
/// forwards it), mirroring Go's `UserService.UpdateUser(ctx, userCtx.ID, …)`.
#[async_trait::async_trait]
pub trait MeMutationServices: Send + Sync {
    /// Mirrors Go `UpdateMe`: partial-merge update of the current user's own
    /// profile (`FirstName` / `LastName` / `PreferLanguage` / `Avatar`).
    /// Returns the updated [`User`].
    async fn update_me(&self, user_id: i64, input: UpdateMeInput) -> Result<User, MeMutationError>;

    /// Mirrors Go `UpdateMyPassword`: verify `old_password`, set
    /// `new_password` for the current user.
    async fn update_my_password(
        &self,
        user_id: i64,
        old_password: String,
        new_password: String,
    ) -> Result<(), MeMutationError>;

    /// Mirrors Go `UnlinkOIDCIdentity`: unlink the OIDC identity `identity_id`
    /// (the raw GraphQL `ID!` wire form) from the current user, after the
    /// ownership + last-identity guards. The Go resolver enforces those guards
    /// against the DB; the host implementation carries them.
    async fn unlink_oidc_identity(
        &self,
        user_id: i64,
        identity_id: String,
    ) -> Result<(), MeMutationError>;
}

/// Resolves the injected [`MeMutationServices`] from the async-graphql context
/// data bag, surfacing the Go-equivalent "me service is not available" message
/// when no service was wired.
pub(crate) fn me_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn MeMutationServices>, String> {
    match ctx.data::<Arc<dyn MeMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(MeMutationError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, ID, Name, Schema, Value};

    use super::*;
    use crate::me::CurrentUser;
    use crate::mutation::MutationRoot;
    use crate::scalars::TimeScalar;
    use crate::user::{User, UserStatus};

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    fn sample_user(user_id: i64) -> User {
        User {
            id: ID::from(format!("gid://conduit/User/{user_id}")),
            created_at: TimeScalar(chrono::DateTime::<chrono::Utc>::default()),
            updated_at: TimeScalar(chrono::DateTime::<chrono::Utc>::default()),
            email: "ada@example.com".to_string(),
            status: UserStatus::Activated,
            prefer_language: "en".to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            avatar: None,
            is_owner: true,
            scopes: Some(vec!["read".to_string()]),
        }
    }

    // ---- in-memory fake --------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeMeMutationServices {
        update_me_calls: Arc<Mutex<Vec<(i64, UpdateMeInput)>>>,
        update_password_calls: Arc<Mutex<Vec<(i64, String, String)>>>,
        unlink_calls: Arc<Mutex<Vec<(i64, String)>>>,
        update_me_error: Option<MeMutationError>,
        update_password_error: Option<MeMutationError>,
        unlink_error: Option<MeMutationError>,
    }

    #[async_trait::async_trait]
    impl MeMutationServices for FakeMeMutationServices {
        async fn update_me(
            &self,
            user_id: i64,
            input: UpdateMeInput,
        ) -> Result<User, MeMutationError> {
            lock(&self.update_me_calls).push((user_id, input.clone()));
            if let Some(err) = &self.update_me_error {
                return Err(err.clone());
            }
            // Mirror the partial merge: apply provided fields onto the base user.
            let mut user = sample_user(user_id);
            if let Some(v) = input.first_name {
                user.first_name = v;
            }
            if let Some(v) = input.last_name {
                user.last_name = v;
            }
            if let Some(v) = input.prefer_language {
                user.prefer_language = v;
            }
            if let Some(v) = input.avatar {
                user.avatar = Some(v);
            }
            Ok(user)
        }

        async fn update_my_password(
            &self,
            user_id: i64,
            old_password: String,
            new_password: String,
        ) -> Result<(), MeMutationError> {
            lock(&self.update_password_calls).push((user_id, old_password, new_password));
            match &self.update_password_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn unlink_oidc_identity(
            &self,
            user_id: i64,
            identity_id: String,
        ) -> Result<(), MeMutationError> {
            lock(&self.unlink_calls).push((user_id, identity_id));
            match &self.unlink_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with(services: FakeMeMutationServices) -> TestSchema {
        let arc: Arc<dyn MeMutationServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    fn request_as_user(query: &str, user_id: i64) -> async_graphql::Request {
        async_graphql::Request::new(query).data(CurrentUser { user_id })
    }

    // ---- resolver: updateMe ---------------------------------------------

    #[tokio::test]
    async fn update_me_applies_partial_merge_and_returns_user() {
        let fake = FakeMeMutationServices::default();
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(request_as_user(
                r#"mutation { updateMe(input: { firstName: "Grace", preferLanguage: "zh" }) { id email firstName preferLanguage } }"#,
                7,
            ))
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let me = match obj.get(&Name::new("updateMe")) {
            Some(v) => v,
            None => panic!("updateMe field missing in {obj:?}"),
        };
        let fields = as_object(me);
        match fields.get(&Name::new("firstName")) {
            Some(Value::String(s)) => assert_eq!(s, "Grace"),
            other => panic!("firstName unexpected: {other:?}"),
        }
        match fields.get(&Name::new("preferLanguage")) {
            Some(Value::String(s)) => assert_eq!(s, "zh"),
            other => panic!("preferLanguage unexpected: {other:?}"),
        }
        // Service saw the resolved user_id + the exact input.
        let calls = lock(&fake.update_me_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 7);
        assert_eq!(calls[0].1.first_name.as_deref(), Some("Grace"));
        assert_eq!(calls[0].1.last_name, None);
    }

    #[tokio::test]
    async fn update_me_surfaces_user_not_found_without_current_user() {
        // Go resolver: no user in context -> "user not found in context".
        let schema = schema_with(FakeMeMutationServices::default());
        let resp = schema
            .execute(r#"mutation { updateMe(input: {}) { id } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("user not found in context"), "msg: {msg}");
    }

    #[tokio::test]
    async fn update_me_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();
        let resp = schema
            .execute(request_as_user(
                r#"mutation { updateMe(input: {}) { id } }"#,
                1,
            ))
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("me service is not available"), "msg: {msg}");
    }

    #[tokio::test]
    async fn update_me_surfaces_wrapped_error() {
        let fake = FakeMeMutationServices {
            update_me_error: Some(MeMutationError::UpdateUser("db down".to_string())),
            ..FakeMeMutationServices::default()
        };
        let schema = schema_with(fake);
        let resp = schema
            .execute(request_as_user(
                r#"mutation { updateMe(input: {}) { id } }"#,
                1,
            ))
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to update user"), "msg: {msg}");
        assert!(msg.contains("db down"), "msg: {msg}");
    }

    // ---- resolver: updateMyPassword -------------------------------------

    #[tokio::test]
    async fn update_my_password_forwards_credentials_and_returns_true() {
        let fake = FakeMeMutationServices::default();
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(request_as_user(
                r#"mutation { updateMyPassword(input: { oldPassword: "old", newPassword: "new" }) }"#,
                9,
            ))
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("updateMyPassword")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("updateMyPassword unexpected: {other:?}"),
        }
        let calls = lock(&fake.update_password_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 9);
        assert_eq!(calls[0].1, "old");
        assert_eq!(calls[0].2, "new");
    }

    #[tokio::test]
    async fn update_my_password_surfaces_wrapped_error() {
        let fake = FakeMeMutationServices {
            update_password_error: Some(MeMutationError::UpdatePassword(
                "wrong password".to_string(),
            )),
            ..FakeMeMutationServices::default()
        };
        let schema = schema_with(fake);
        let resp = schema
            .execute(request_as_user(
                r#"mutation { updateMyPassword(input: { oldPassword: "x", newPassword: "y" }) }"#,
                1,
            ))
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to update password"), "msg: {msg}");
        assert!(msg.contains("wrong password"), "msg: {msg}");
    }

    #[tokio::test]
    async fn update_my_password_surfaces_user_not_found_without_current_user() {
        let schema = schema_with(FakeMeMutationServices::default());
        let resp = schema
            .execute(
                r#"mutation { updateMyPassword(input: { oldPassword: "x", newPassword: "y" }) }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("user not found in context"), "msg: {msg}");
    }

    // ---- resolver: unlinkOIDCIdentity -----------------------------------

    #[tokio::test]
    async fn unlink_oidc_identity_forwards_id_and_returns_true() {
        let fake = FakeMeMutationServices::default();
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(request_as_user(
                r#"mutation { unlinkOIDCIdentity(id: "gid://conduit/OIDCIdentity/5") }"#,
                3,
            ))
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("unlinkOIDCIdentity")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("unlinkOIDCIdentity unexpected: {other:?}"),
        }
        let calls = lock(&fake.unlink_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 3);
        assert_eq!(calls[0].1, "gid://conduit/OIDCIdentity/5");
    }

    #[tokio::test]
    async fn unlink_oidc_identity_surfaces_wrapped_error() {
        let fake = FakeMeMutationServices {
            unlink_error: Some(MeMutationError::UnlinkIdentity(
                "please set a local password before unlinking your last OIDC identity".to_string(),
            )),
            ..FakeMeMutationServices::default()
        };
        let schema = schema_with(fake);
        let resp = schema
            .execute(request_as_user(
                r#"mutation { unlinkOIDCIdentity(id: "gid://conduit/OIDCIdentity/5") }"#,
                1,
            ))
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to unlink identity"), "msg: {msg}");
    }

    #[tokio::test]
    async fn unlink_oidc_identity_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();
        let resp = schema
            .execute(request_as_user(
                r#"mutation { unlinkOIDCIdentity(id: "gid://conduit/OIDCIdentity/5") }"#,
                1,
            ))
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("me service is not available"), "msg: {msg}");
    }

    // ---- SDL shape parity -----------------------------------------------

    fn snapshot_text() -> Result<String, Box<dyn std::error::Error>> {
        std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| format!("snapshot read failed: {err}").into())
    }

    #[test]
    fn sdl_contains_me_mutation_slice() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn MeMutationServices> = Arc::new(FakeMeMutationServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();

        for expected in [
            "input UpdateMeInput {",
            "input UpdateMyPasswordInput {",
            "updateMe(input: UpdateMeInput!): User!",
            "updateMyPassword(input: UpdateMyPasswordInput!): Boolean!",
            "unlinkOIDCIdentity(id: ID!): Boolean!",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }
        Ok(())
    }

    /// Cross-check the two input blocks against the captured snapshot exactly.
    #[test]
    fn sdl_input_blocks_match_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn MeMutationServices> = Arc::new(FakeMeMutationServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();
        let snapshot = snapshot_text()?;
        for header in ["input UpdateMeInput", "input UpdateMyPasswordInput"] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }
}

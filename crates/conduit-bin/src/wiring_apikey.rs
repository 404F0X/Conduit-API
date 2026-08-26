//! ADPT-APIKEY (formerly GAP-I) — host adapter wiring the admin GraphQL
//! **ApiKey** domain to the configured [`ApiKeyRepo`].
//!
//! The resolver layer (`conduit-admin-graphql/src/apikey.rs`) is already
//! complete: it declares the GraphQL types plus two host-injected traits,
//! [`ApiKeyQueryServices`] (backs `Query.apiKeys`) and
//! [`ApiKeyMutationServices`] (backs `createAPIKey` / `updateAPIKey` /
//! `updateAPIKeyStatus` / `rotateAPIKey` / `updateAPIKeyProfiles` /
//! `bulk{Disable,Enable,Archive}APIKeys`). This file provides the single
//! concrete [`ApiKeyServiceAdapter`] that implements both traits over the DB.
//!
//! ## Go parity anchors (`conduit/internal/server/biz/api_key.go`)
//!
//!   - **Key generation** (`GenerateAPIKey`, api_key.go:168): `prefix + "-" +
//!     hex(32 random bytes)` → 64 hex chars. Ported verbatim by
//!     [`conduit_auth::apikey::generate_api_key`], which we reuse. The prefix is
//!     the configured `server.api.auth.key_prefix` (default `"conduit"`,
//!     conf.go:188).
//!   - **Key visibility / masking**: there is NO masking on the admin surface.
//!     The Go `APIKey.key: String!` GraphQL field returns the raw ent `Key`
//!     column on every authenticated admin read (the "echoed once" pattern does
//!     NOT apply here). The frontend renders the full key. We therefore return
//!     the full `key` from every query/mutation — inventing a mask would break
//!     parity.
//!   - **CreateAPIKey** (api_key.go:309): reject an explicit `noauth` type
//!     ("noauth type API key is reserved") → generate the key → per-project
//!     LIVE-name duplicate check (repo enforced) → column defaults: type `user`,
//!     status `enabled`, scopes `[read_channels, write_requests]` for `user`,
//!     profiles empty struct `{}`. `service_account` honors the supplied scopes
//!     (or `[]`); `user` ignores them and takes the default.
//!   - **UpdateAPIKey** (api_key.go:396): `user` rejects any scope mutation;
//!     `noauth` rejects any update; rename duplicate probe (repo enforced);
//!     `service_account` scope set/append/clear (clear wins, applied last).
//!   - **UpdateAPIKeyStatus** (api_key.go:477): `noauth` rejected; no transition
//!     restriction (archived → enabled is allowed).
//!   - **RotateAPIKey** (api_key.go:818): `noauth` rejected; ONLY the `key`
//!     column changes (status/name/scopes/profiles preserved).
//!   - **UpdateAPIKeyProfiles** (api_key.go:503): `noauth` rejected; profile
//!     names non-empty + case-insensitive unique; active profile must exist.
//!   - **bulkUpdateAPIKeyStatus** (api_key.go:751): empty ids is a no-op; every
//!     id must resolve; NO id may be `noauth`-type; bulk SetStatus.
//!
//! ## GraphQL id encoding
//!
//! Ids on the wire are Conduit API GUIDs `gid://conduit/<Type>/<n>` (Go
//! `objects.GUID`). This adapter encodes `APIKey`/`Project`/`User` ids in that
//! form on the way out and decodes them (accepting a bare numeric too, matching
//! `GUID.UnmarshalGQL` + the `ModelCrudAdapter` precedent) on the way in.
//!
//! ## Divergences (documented, not silent)
//!
//!   - **`Query.apiKeys` `where`**: filtered in memory over the loaded rows
//!     (mirrors `ModelCrudAdapter`). Covered predicates: `and`/`or`/`not`,
//!     `projectID`/`userID` (+ `isNil`/`notNil`), `status`, `type`, and the
//!     `name`/`key` string families. Time/id and `has<Edge>` predicates are not
//!     lowered (they are absent from the admin api-keys list UI).

use std::sync::Arc;

use async_graphql::ID;
use async_trait::async_trait;

use conduit_admin_graphql::apikey::{
    APIKey, APIKeyAccessScope, APIKeyConnection, APIKeyConnectionArgs, APIKeyEdge, APIKeyOrderTerm,
    APIKeyProfile, APIKeyProfileInput, APIKeyProfiles, APIKeyQuota, APIKeyQuotaCalendarDuration,
    APIKeyQuotaCalendarDurationUnit, APIKeyQuotaInput, APIKeyQuotaPastDuration,
    APIKeyQuotaPastDurationUnit, APIKeyQuotaPeriod, APIKeyQuotaPeriodType, APIKeyServiceError,
    APIKeyStatus, APIKeyType, APIKeyWhereInput, ApiKeyMutationServices, ApiKeyQueryServices,
    ChannelTagsMatchMode, CreateAPIKeyInput, UpdateAPIKeyInput, UpdateAPIKeyProfilesInput,
};
use conduit_admin_graphql::channel::{ModelMapping as GqlModelMapping, OrderDirection};
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::scalars::{CursorScalar, DecimalScalar, TimeScalar};
use conduit_auth::apikey::generate_api_key;
use conduit_core::objects::apikey::{
    APIKeyProfile as CoreApiKeyProfile, APIKeyProfiles as CoreApiKeyProfiles,
    APIKeyQuota as CoreApiKeyQuota, APIKeyQuotaCalendarDuration as CoreCalDuration,
    APIKeyQuotaPastDuration as CorePastDuration, APIKeyQuotaPeriod as CoreQuotaPeriod,
    ModelMapping as CoreModelMapping,
};
use conduit_db::repo::ApiKeyRepo;
use conduit_db::repo::api_key_repo::{
    CreateApiKeyInput as RepoCreateApiKeyInput, ListApiKeysQuery,
    UpdateApiKeyInput as RepoUpdateApiKeyInput,
};
use conduit_db::row::ApiKeyRow;
use conduit_db::{PolicyContext, Principal, RepoError, RequestContext};

// ===========================================================================
// Adapter
// ===========================================================================

/// Concrete host adapter implementing both api-key service traits over
/// [`ApiKeyRepo`]. The host wires one instance as two trait objects
/// (`Arc<dyn ApiKeyQueryServices>` + `Arc<dyn ApiKeyMutationServices>`).
pub struct ApiKeyServiceAdapter {
    repo: Arc<dyn ApiKeyRepo>,
    /// Configured api-key prefix (`server.api.auth.key_prefix`, Go default `ah`).
    key_prefix: String,
}

impl ApiKeyServiceAdapter {
    pub fn new(repo: Arc<dyn ApiKeyRepo>, key_prefix: String) -> Self {
        Self { repo, key_prefix }
    }

    /// System boot has no authenticated principal; repo access uses the `Test`
    /// principal, which `conduit-db` policy treats as a trusted bypass (matching
    /// Go's pre-auth system paths). Same convention as `wiring::boot_request_context`.
    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    /// Materialize every live api-key row, paging through the repo in generous
    /// windows (the table is admin-scale). Mirrors Go's ent `.All(ctx)`.
    async fn load_project_live(
        &self,
        scope: &APIKeyAccessScope,
    ) -> Result<Vec<ApiKeyRow>, APIKeyServiceError> {
        let ctx = Self::ctx();
        let project_id = decode_gql_id(&scope.project_id)
            .ok_or_else(|| APIKeyServiceError::Query("invalid selected project id".to_string()))?;
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListApiKeysQuery {
                limit: PAGE,
                offset,
                ..Default::default()
            };
            let result = self
                .repo
                .list_api_keys_by_project(&ctx, &project_id, &query)
                .await
                .map_err(|e| APIKeyServiceError::Query(e.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset = offset.saturating_add(PAGE);
        }
        Ok(rows)
    }

    fn ensure_row_in_project(
        scope: &APIKeyAccessScope,
        row: &ApiKeyRow,
    ) -> Result<(), APIKeyServiceError> {
        let project_id =
            decode_gql_id(&scope.project_id).ok_or_else(|| APIKeyServiceError::NotFound)?;
        if row.project_id == project_id {
            Ok(())
        } else {
            // Do not reveal whether an ID exists in another Project.
            Err(APIKeyServiceError::NotFound)
        }
    }

    /// Shared implementation of the three bulk status mutations. Mirrors Go
    /// `bulkUpdateAPIKeyStatus` (api_key.go:751): empty ids → no-op; every id
    /// must resolve to a distinct row; NO id may be `noauth`-type; bulk SetStatus.
    async fn bulk_update_status(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
        status_wire: &str,
        action: &str,
    ) -> Result<(), APIKeyServiceError> {
        if ids.is_empty() {
            return Ok(());
        }
        let ctx = Self::ctx();

        // Distinct decoded ids — an undecodable id contributes no row, so it
        // fails the count check below exactly like a missing row (Go IN(...) also
        // dedups, so duplicate ids fail the `count == len(ids)` check too).
        let mut distinct: Vec<String> = Vec::new();
        for id in &ids {
            if let Some(db_id) = decode_gql_id(id)
                && !distinct.contains(&db_id)
            {
                distinct.push(db_id);
            }
        }

        let mut rows = Vec::new();
        for db_id in &distinct {
            if let Some(row) = self
                .repo
                .find_api_key_by_id(&ctx, db_id)
                .await
                .map_err(|e| APIKeyServiceError::BulkUpdate(action.to_owned(), e.to_string()))?
            {
                Self::ensure_row_in_project(scope, &row).map_err(|_| {
                    APIKeyServiceError::BulkUpdate(
                        action.to_owned(),
                        "API key not found".to_string(),
                    )
                })?;
                rows.push(row);
            }
        }

        // Every requested id must resolve (Go `count != len(ids)`).
        if rows.len() != ids.len() {
            return Err(APIKeyServiceError::BulkUpdate(
                action.to_owned(),
                format!(
                    "expected to find {} API keys, but found {}",
                    ids.len(),
                    rows.len()
                ),
            ));
        }
        // No `noauth`-type key may be bulk-updated (Go api_key.go:770-779).
        if rows
            .iter()
            .any(|r| key_type_from_wire(&r.key_type) == APIKeyType::Noauth)
        {
            return Err(APIKeyServiceError::BulkUpdate(
                action.to_owned(),
                format!("noauth type API key cannot be bulk {action}d"),
            ));
        }

        let now = chrono::Utc::now().to_rfc3339();
        for row in &rows {
            self.repo
                .update_api_key(
                    &ctx,
                    &row.id,
                    RepoUpdateApiKeyInput {
                        status: Some(status_wire.to_owned()),
                        updated_at: now.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| APIKeyServiceError::BulkUpdate(action.to_owned(), e.to_string()))?;
        }
        Ok(())
    }
}

// ===========================================================================
// Query trait
// ===========================================================================

#[async_trait]
impl ApiKeyQueryServices for ApiKeyServiceAdapter {
    async fn api_keys(
        &self,
        scope: &APIKeyAccessScope,
        args: APIKeyConnectionArgs,
    ) -> Result<APIKeyConnection, APIKeyServiceError> {
        let rows = self.load_project_live(scope).await?;

        // In-memory `where` filter (covered predicates: see module doc).
        let mut rows: Vec<ApiKeyRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| api_key_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Ordering: the crate lowered `CREATED_AT` → `Id` (ent DefaultAPIKeyOrder).
        // The repo returns created_at-asc; re-sort for any explicit selection.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    APIKeyOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    APIKeyOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let nodes: Vec<APIKey> = rows.into_iter().map(row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme (matching
        // `connection_from_offset_page`). A malformed `after` degrades to offset 0.
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

        Ok(APIKeyConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(APIKeyEdge {
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

    async fn api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
    ) -> Result<Option<APIKey>, APIKeyServiceError> {
        let db_id = decode_gql_id(id).ok_or(APIKeyServiceError::NotFound)?;
        let row = self
            .repo
            .find_api_key_by_id(&Self::ctx(), &db_id)
            .await
            .map_err(|error| APIKeyServiceError::Query(error.to_string()))?;
        match row {
            Some(row) => {
                Self::ensure_row_in_project(scope, &row)?;
                Ok(Some(row_to_gql(row)))
            }
            None => Ok(None),
        }
    }
}

// ===========================================================================
// Mutation trait
// ===========================================================================

#[async_trait]
impl ApiKeyMutationServices for ApiKeyServiceAdapter {
    async fn create_api_key(
        &self,
        scope: &APIKeyAccessScope,
        current_user_id: Option<i64>,
        input: CreateAPIKeyInput,
    ) -> Result<APIKey, APIKeyServiceError> {
        let ctx = Self::ctx();

        // Go api_key.go:316-319: an explicit `noauth` type is reserved.
        if matches!(input.key_type, Some(APIKeyType::Noauth)) {
            return Err(APIKeyServiceError::NoauthReserved);
        }
        let key_type = input.key_type.unwrap_or(APIKeyType::User);

        let generated = generate_api_key(&self.key_prefix)
            .map_err(|e| APIKeyServiceError::Create(e.to_string()))?;
        let project_db = decode_gql_id(input.project_id.as_str())
            .ok_or_else(|| APIKeyServiceError::Create("invalid project id".to_owned()))?;
        let selected_project = decode_gql_id(&scope.project_id)
            .ok_or_else(|| APIKeyServiceError::Create("invalid selected project id".to_owned()))?;
        if project_db != selected_project {
            return Err(APIKeyServiceError::Create(
                "API key project must match the selected project".to_owned(),
            ));
        }

        // Column defaults: `user` takes the schema default scopes and IGNORES the
        // supplied ones; `service_account` uses the supplied scopes (or `[]`).
        let scopes = match key_type {
            APIKeyType::User => default_user_scopes(),
            APIKeyType::ServiceAccount => input.scopes.unwrap_or_default(),
            APIKeyType::Noauth => Vec::new(), // unreachable (rejected above)
        };

        if let Some(initial_profiles) = input.profiles.as_ref() {
            let profiles = initial_profiles.profiles.as_deref().unwrap_or(&[]);
            let mut seen = std::collections::HashSet::new();
            for profile in profiles {
                let normalized = profile.name.trim().to_lowercase();
                if normalized.is_empty() {
                    return Err(APIKeyServiceError::ProfileNameEmpty);
                }
                if !seen.insert(normalized) {
                    return Err(APIKeyServiceError::DuplicateProfileName(
                        profile.name.clone(),
                    ));
                }
            }
            if (!profiles.is_empty() || !initial_profiles.active_profile.is_empty())
                && !profiles
                    .iter()
                    .any(|profile| profile.name == initial_profiles.active_profile)
            {
                return Err(APIKeyServiceError::ActiveProfileMissing(
                    initial_profiles.active_profile.clone(),
                ));
            }
        }

        let profiles = input
            .profiles
            .clone()
            .map(profiles_input_to_core)
            .transpose()
            .map_err(|error| APIKeyServiceError::Create(error.to_string()))?;
        let profiles = profiles
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| APIKeyServiceError::Create(error.to_string()))?;

        let now = chrono::Utc::now().to_rfc3339();
        let repo_input = RepoCreateApiKeyInput {
            id: String::new(), // the database owns the generated PK
            user_id: current_user_id.map(|id| id.to_string()),
            project_id: project_db,
            name: input.name.clone(),
            key: generated,
            key_type: key_type_to_wire(key_type).to_owned(),
            scopes,
            profiles,
            created_at: now.clone(),
        };
        let row = self
            .repo
            .create_api_key(&ctx, repo_input)
            .await
            .map_err(|e| match e {
                // Go `xerrors.DuplicateNameError("API Key", name)`.
                RepoError::NameConflict => APIKeyServiceError::DuplicateName(input.name.clone()),
                other => APIKeyServiceError::Create(other.to_string()),
            })?;

        Ok(row_to_gql(row))
    }

    async fn update_api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        input: UpdateAPIKeyInput,
    ) -> Result<APIKey, APIKeyServiceError> {
        let ctx = Self::ctx();
        let db_id = decode_gql_id(id)
            .ok_or_else(|| APIKeyServiceError::Update(APIKeyServiceError::NotFound.to_string()))?;

        // Go `client.APIKey.Get` failure → "failed to get API key".
        let existing = self
            .repo
            .find_api_key_by_id(&ctx, &db_id)
            .await
            .map_err(|e| APIKeyServiceError::Update(e.to_string()))?
            .ok_or_else(|| APIKeyServiceError::Update(APIKeyServiceError::NotFound.to_string()))?;
        Self::ensure_row_in_project(scope, &existing)
            .map_err(|error| APIKeyServiceError::Update(error.to_string()))?;
        let key_type = key_type_from_wire(&existing.key_type);

        // Go api_key.go:407-411: `user` rejects any scope mutation (length-checked,
        // so empty `scopes` + no append + no clear passes).
        if key_type == APIKeyType::User
            && (input.scopes.as_deref().is_some_and(|s| !s.is_empty())
                || input
                    .append_scopes
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
                || input.clear_scopes.unwrap_or(false))
        {
            return Err(APIKeyServiceError::UserTypeScopesImmutable);
        }
        // Go api_key.go:413-415: `noauth` rejects any update.
        if key_type == APIKeyType::Noauth {
            return Err(APIKeyServiceError::NoauthNotUpdatable);
        }

        // Resolve the `service_account` scope set/append/clear into a final vector.
        // Go composes SetScopes → AppendScopes → ClearScopes on one builder, so
        // clear (applied last) wins.
        let new_scopes: Option<Vec<String>> = if key_type == APIKeyType::ServiceAccount {
            let mut base = existing.scopes.clone();
            let mut touched = false;
            if let Some(set) = &input.scopes
                && !set.is_empty()
            {
                base = set.clone();
                touched = true;
            }
            if let Some(append) = &input.append_scopes
                && !append.is_empty()
            {
                base.extend(append.iter().cloned());
                touched = true;
            }
            if input.clear_scopes.unwrap_or(false) {
                base = Vec::new();
                touched = true;
            }
            if touched { Some(base) } else { None }
        } else {
            None
        };

        let now = chrono::Utc::now().to_rfc3339();
        let repo_input = RepoUpdateApiKeyInput {
            name: input.name.clone(),
            scopes: new_scopes,
            updated_at: now,
            ..Default::default()
        };
        let row = self
            .repo
            .update_api_key(&ctx, &db_id, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => {
                    APIKeyServiceError::DuplicateName(input.name.clone().unwrap_or_default())
                }
                RepoError::NotFound(_) => {
                    APIKeyServiceError::Update(APIKeyServiceError::NotFound.to_string())
                }
                other => APIKeyServiceError::Update(other.to_string()),
            })?;
        Ok(row_to_gql(row))
    }

    async fn update_api_key_status(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        status: APIKeyStatus,
    ) -> Result<APIKey, APIKeyServiceError> {
        let ctx = Self::ctx();
        let db_id = decode_gql_id(id).ok_or_else(|| {
            APIKeyServiceError::UpdateStatus(APIKeyServiceError::NotFound.to_string())
        })?;

        let existing = self
            .repo
            .find_api_key_by_id(&ctx, &db_id)
            .await
            .map_err(|e| APIKeyServiceError::UpdateStatus(e.to_string()))?
            .ok_or_else(|| {
                APIKeyServiceError::UpdateStatus(APIKeyServiceError::NotFound.to_string())
            })?;
        Self::ensure_row_in_project(scope, &existing)
            .map_err(|error| APIKeyServiceError::UpdateStatus(error.to_string()))?;
        // Go api_key.go:485-487: `noauth` rejected.
        if key_type_from_wire(&existing.key_type) == APIKeyType::Noauth {
            return Err(APIKeyServiceError::NoauthStatusNotUpdatable);
        }

        // No transition restriction (archived → enabled allowed).
        let now = chrono::Utc::now().to_rfc3339();
        let row = self
            .repo
            .update_api_key(
                &ctx,
                &db_id,
                RepoUpdateApiKeyInput {
                    status: Some(status_to_wire(status).to_owned()),
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => {
                    APIKeyServiceError::UpdateStatus(APIKeyServiceError::NotFound.to_string())
                }
                other => APIKeyServiceError::UpdateStatus(other.to_string()),
            })?;
        Ok(row_to_gql(row))
    }

    async fn rotate_api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
    ) -> Result<APIKey, APIKeyServiceError> {
        let ctx = Self::ctx();
        let db_id = decode_gql_id(id)
            .ok_or_else(|| APIKeyServiceError::Rotate(APIKeyServiceError::NotFound.to_string()))?;

        let existing = self
            .repo
            .find_api_key_by_id(&ctx, &db_id)
            .await
            .map_err(|e| APIKeyServiceError::Rotate(e.to_string()))?
            .ok_or_else(|| APIKeyServiceError::Rotate(APIKeyServiceError::NotFound.to_string()))?;
        Self::ensure_row_in_project(scope, &existing)
            .map_err(|error| APIKeyServiceError::Rotate(error.to_string()))?;
        // Go api_key.go:826-828: `noauth` rejected.
        if key_type_from_wire(&existing.key_type) == APIKeyType::Noauth {
            return Err(APIKeyServiceError::NoauthNotRotatable);
        }

        // Go rotates by setting ONLY the `key` column. The repo's
        // `UpdateApiKeyInput` has no `key` field, so we issue the targeted UPDATE
        // directly (task-sanctioned: repo-missing method → hand-written sqlx SQL).
        let new_key = generate_api_key(&self.key_prefix)
            .map_err(|e| APIKeyServiceError::Rotate(e.to_string()))?;
        self.repo
            .rotate_api_key(&ctx, &db_id, &new_key)
            .await
            .map_err(|e| APIKeyServiceError::Rotate(e.to_string()))?;

        let row = self
            .repo
            .find_api_key_by_id(&ctx, &db_id)
            .await
            .map_err(|e| APIKeyServiceError::Rotate(e.to_string()))?
            .ok_or_else(|| APIKeyServiceError::Rotate(APIKeyServiceError::NotFound.to_string()))?;
        Ok(row_to_gql(row))
    }

    async fn update_api_key_profiles(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        input: UpdateAPIKeyProfilesInput,
    ) -> Result<APIKey, APIKeyServiceError> {
        let ctx = Self::ctx();
        let db_id = decode_gql_id(id).ok_or_else(|| {
            APIKeyServiceError::UpdateProfiles(APIKeyServiceError::NotFound.to_string())
        })?;

        let existing = self
            .repo
            .find_api_key_by_id(&ctx, &db_id)
            .await
            .map_err(|e| APIKeyServiceError::UpdateProfiles(e.to_string()))?
            .ok_or_else(|| {
                APIKeyServiceError::UpdateProfiles(APIKeyServiceError::NotFound.to_string())
            })?;
        Self::ensure_row_in_project(scope, &existing)
            .map_err(|error| APIKeyServiceError::UpdateProfiles(error.to_string()))?;
        // Go api_key.go:511-513: `noauth` rejected.
        if key_type_from_wire(&existing.key_type) == APIKeyType::Noauth {
            return Err(APIKeyServiceError::NoauthProfilesNotUpdatable);
        }

        // Go `validateProfileNames`: non-empty + case-insensitive unique.
        let profiles = input.profiles.as_deref().unwrap_or(&[]);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in profiles {
            let name_lower = p.name.trim().to_lowercase();
            if name_lower.is_empty() {
                return Err(APIKeyServiceError::ProfileNameEmpty);
            }
            if !seen.insert(name_lower) {
                return Err(APIKeyServiceError::DuplicateProfileName(p.name.clone()));
            }
        }
        // Go `validateActiveProfile`: the active profile must exist in the list.
        if !input.active_profile.is_empty()
            && !profiles.iter().any(|p| p.name == input.active_profile)
        {
            return Err(APIKeyServiceError::ActiveProfileMissing(
                input.active_profile.clone(),
            ));
        }

        let proposed_profiles = profiles_input_to_core(input)?;

        // Lower to the canonical Go on-disk shape (core objects) and serialize.
        let profiles_json = serde_json::to_value(&proposed_profiles)
            .map_err(|error| APIKeyServiceError::UpdateProfiles(error.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();
        let row = self
            .repo
            .update_api_key(
                &ctx,
                &db_id,
                RepoUpdateApiKeyInput {
                    profiles: Some(profiles_json),
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => {
                    APIKeyServiceError::UpdateProfiles(APIKeyServiceError::NotFound.to_string())
                }
                other => APIKeyServiceError::UpdateProfiles(other.to_string()),
            })?;
        Ok(row_to_gql(row))
    }

    async fn bulk_disable_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError> {
        self.bulk_update_status(scope, ids, "disabled", "disable")
            .await
    }

    async fn bulk_enable_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError> {
        self.bulk_update_status(scope, ids, "enabled", "enable")
            .await
    }

    async fn bulk_archive_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError> {
        self.bulk_update_status(scope, ids, "archived", "archive")
            .await
    }
}

// ===========================================================================
// Row → GraphQL conversion
// ===========================================================================

/// Convert a DB [`ApiKeyRow`] into the GraphQL [`APIKey`]. The full `key` is
/// returned (no masking — Go parity, see module doc).
pub(crate) fn row_to_gql(row: ApiKeyRow) -> APIKey {
    // A malformed profiles JSON degrades to the empty struct rather than failing
    // the whole read (a single bad row must not black out the list).
    let core_profiles: CoreApiKeyProfiles =
        serde_json::from_value(row.profiles.clone()).unwrap_or_default();

    APIKey {
        id: apikey_gid(&row.id),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        user_id: row.user_id.as_deref().map(user_gid),
        project_id: project_gid(&row.project_id),
        key: row.key,
        name: row.name,
        key_type: key_type_from_wire(&row.key_type),
        status: status_from_wire(&row.status),
        scopes: Some(row.scopes),
        // Go `APIKey.Profiles` is a value type (never null); always present.
        profiles: Some(core_profiles_to_gql(core_profiles)),
    }
}

fn core_profiles_to_gql(core: CoreApiKeyProfiles) -> APIKeyProfiles {
    let profiles: Vec<APIKeyProfile> = core.profiles.into_iter().map(core_profile_to_gql).collect();
    APIKeyProfiles {
        active_profile: core.active_profile,
        profiles: if profiles.is_empty() {
            None
        } else {
            Some(profiles)
        },
    }
}

fn core_profile_to_gql(p: CoreApiKeyProfile) -> APIKeyProfile {
    APIKeyProfile {
        name: p.name,
        model_mappings: if p.model_mappings.is_empty() {
            None
        } else {
            Some(
                p.model_mappings
                    .into_iter()
                    .map(|m| GqlModelMapping {
                        from: m.from,
                        to: m.to,
                    })
                    .collect(),
            )
        },
        channel_ids: if p.channel_ids.is_empty() {
            None
        } else {
            Some(p.channel_ids)
        },
        channel_tags: if p.channel_tags.is_empty() {
            None
        } else {
            Some(p.channel_tags)
        },
        channel_tags_match_mode: p
            .channel_tags_match_mode
            .as_deref()
            .and_then(wire_to_gql_tags_mode),
        model_ids: if p.model_ids.is_empty() {
            None
        } else {
            Some(p.model_ids)
        },
        valid_from: p.valid_from.map(TimeScalar),
        valid_until: p.valid_until.map(TimeScalar),
        quota: p.quota.map(core_quota_to_gql),
        load_balance_strategy: p.load_balance_strategy,
        max_concurrent_requests: p.max_concurrent_requests.map(i64::from),
    }
}

fn core_quota_to_gql(q: CoreApiKeyQuota) -> APIKeyQuota {
    APIKeyQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(DecimalScalar),
        period: APIKeyQuotaPeriod {
            period_type: wire_to_gql_period_type(&q.period.r#type),
            past_duration: q.period.past_duration.map(|pd| APIKeyQuotaPastDuration {
                value: pd.value,
                unit: wire_to_gql_past_unit(&pd.unit),
            }),
            calendar_duration: q
                .period
                .calendar_duration
                .map(|cd| APIKeyQuotaCalendarDuration {
                    unit: wire_to_gql_cal_unit(&cd.unit),
                }),
        },
    }
}

// ===========================================================================
// GraphQL profiles input → canonical JSON (write path)
// ===========================================================================

/// Lower the GraphQL profiles input into the Go on-disk JSON shape by building
/// the canonical `conduit_core` objects and serializing them (guarantees the
/// exact camelCase + omitempty layout Go persists and the read path parses).
fn profiles_input_to_core(
    input: UpdateAPIKeyProfilesInput,
) -> Result<CoreApiKeyProfiles, APIKeyServiceError> {
    let profiles = input.profiles.unwrap_or_default();
    if profiles.iter().any(|profile| {
        profile
            .max_concurrent_requests
            .is_some_and(|value| value < 0)
    }) {
        return Err(APIKeyServiceError::UpdateProfiles(
            "maxConcurrentRequests must be zero or greater".to_owned(),
        ));
    }
    let core = CoreApiKeyProfiles {
        active_profile: input.active_profile,
        profiles: profiles
            .into_iter()
            .map(gql_profile_input_to_core)
            .collect(),
    };
    for profile in &core.profiles {
        if let (Some(from), Some(until)) = (profile.valid_from, profile.valid_until)
            && until <= from
        {
            return Err(APIKeyServiceError::UpdateProfiles(
                "profile validUntil must be later than validFrom".to_owned(),
            ));
        }
    }
    Ok(core)
}

fn gql_profile_input_to_core(p: APIKeyProfileInput) -> CoreApiKeyProfile {
    CoreApiKeyProfile {
        name: p.name,
        model_mappings: p
            .model_mappings
            .unwrap_or_default()
            .into_iter()
            .map(|m| CoreModelMapping {
                from: m.from,
                to: m.to,
            })
            .collect(),
        quota: p.quota.map(gql_quota_input_to_core),
        load_balance_strategy: p.load_balance_strategy,
        max_concurrent_requests: p
            .max_concurrent_requests
            .and_then(|value| u32::try_from(value).ok()),
        channel_ids: p.channel_ids.unwrap_or_default(),
        channel_tags: p.channel_tags.unwrap_or_default(),
        channel_tags_match_mode: p
            .channel_tags_match_mode
            .map(|m| gql_tags_mode_to_wire(m).to_owned()),
        model_ids: p.model_ids.unwrap_or_default(),
        valid_from: p.valid_from.map(|value| value.0),
        valid_until: p.valid_until.map(|value| value.0),
    }
}

fn gql_quota_input_to_core(q: APIKeyQuotaInput) -> CoreApiKeyQuota {
    CoreApiKeyQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(|c| c.0),
        period: CoreQuotaPeriod {
            r#type: gql_period_type_to_wire(q.period.period_type).to_owned(),
            past_duration: q.period.past_duration.map(|pd| CorePastDuration {
                value: pd.value,
                unit: gql_past_unit_to_wire(pd.unit).to_owned(),
            }),
            calendar_duration: q.period.calendar_duration.map(|cd| CoreCalDuration {
                unit: gql_cal_unit_to_wire(cd.unit).to_owned(),
            }),
        },
    }
}

// ===========================================================================
// Enum <-> wire-string mappings (wire strings are the Go/GraphQL bound values)
// ===========================================================================

fn key_type_to_wire(t: APIKeyType) -> &'static str {
    match t {
        APIKeyType::User => "user",
        APIKeyType::ServiceAccount => "service_account",
        APIKeyType::Noauth => "noauth",
    }
}

fn key_type_from_wire(s: &str) -> APIKeyType {
    match s {
        "service_account" => APIKeyType::ServiceAccount,
        "noauth" => APIKeyType::Noauth,
        _ => APIKeyType::User,
    }
}

fn status_to_wire(s: APIKeyStatus) -> &'static str {
    match s {
        APIKeyStatus::Enabled => "enabled",
        APIKeyStatus::Disabled => "disabled",
        APIKeyStatus::Archived => "archived",
    }
}

fn status_from_wire(s: &str) -> APIKeyStatus {
    match s {
        "disabled" => APIKeyStatus::Disabled,
        "archived" => APIKeyStatus::Archived,
        _ => APIKeyStatus::Enabled,
    }
}

fn gql_period_type_to_wire(t: APIKeyQuotaPeriodType) -> &'static str {
    match t {
        APIKeyQuotaPeriodType::AllTime => "all_time",
        APIKeyQuotaPeriodType::PastDuration => "past_duration",
        APIKeyQuotaPeriodType::CalendarDuration => "calendar_duration",
    }
}

fn wire_to_gql_period_type(s: &str) -> APIKeyQuotaPeriodType {
    match s {
        "past_duration" => APIKeyQuotaPeriodType::PastDuration,
        "calendar_duration" => APIKeyQuotaPeriodType::CalendarDuration,
        _ => APIKeyQuotaPeriodType::AllTime,
    }
}

fn gql_past_unit_to_wire(u: APIKeyQuotaPastDurationUnit) -> &'static str {
    match u {
        APIKeyQuotaPastDurationUnit::Minute => "minute",
        APIKeyQuotaPastDurationUnit::Hour => "hour",
        APIKeyQuotaPastDurationUnit::Day => "day",
    }
}

fn wire_to_gql_past_unit(s: &str) -> APIKeyQuotaPastDurationUnit {
    match s {
        "hour" => APIKeyQuotaPastDurationUnit::Hour,
        "day" => APIKeyQuotaPastDurationUnit::Day,
        _ => APIKeyQuotaPastDurationUnit::Minute,
    }
}

fn gql_cal_unit_to_wire(u: APIKeyQuotaCalendarDurationUnit) -> &'static str {
    match u {
        APIKeyQuotaCalendarDurationUnit::Day => "day",
        APIKeyQuotaCalendarDurationUnit::Month => "month",
    }
}

fn wire_to_gql_cal_unit(s: &str) -> APIKeyQuotaCalendarDurationUnit {
    match s {
        "month" => APIKeyQuotaCalendarDurationUnit::Month,
        _ => APIKeyQuotaCalendarDurationUnit::Day,
    }
}

fn gql_tags_mode_to_wire(m: ChannelTagsMatchMode) -> &'static str {
    match m {
        ChannelTagsMatchMode::Any => "any",
        ChannelTagsMatchMode::All => "all",
        ChannelTagsMatchMode::None => "none",
    }
}

fn wire_to_gql_tags_mode(s: &str) -> Option<ChannelTagsMatchMode> {
    match s {
        "any" => Some(ChannelTagsMatchMode::Any),
        "all" => Some(ChannelTagsMatchMode::All),
        "none" => Some(ChannelTagsMatchMode::None),
        _ => None,
    }
}

fn default_user_scopes() -> Vec<String> {
    vec!["read_channels".to_owned(), "write_requests".to_owned()]
}

// ===========================================================================
// GUID id encode / decode
// ===========================================================================

fn apikey_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/APIKey/{id}"))
}

fn project_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/Project/{id}"))
}

fn user_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/User/{id}"))
}

/// Decode a GraphQL `ID` into the numeric DB id string. Accepts the typed GUID
/// wire form `gid://conduit/<Type>/<n>` OR a bare numeric string (mirroring Go
/// `GUID.UnmarshalGQL` + the `ModelCrudAdapter` precedent). Returns `None` for
/// anything else (the caller maps that to a not-found).
fn decode_gql_id(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix("gid://conduit/") {
        let id = rest.rsplit('/').next()?;
        if id.is_empty() || id.parse::<i64>().is_err() {
            return None;
        }
        Some(id.to_owned())
    } else if raw.parse::<i64>().is_ok() {
        Some(raw.to_owned())
    } else {
        None
    }
}

// ===========================================================================
// `where` predicate matcher (in-memory; covered subset — see module doc)
// ===========================================================================

/// Evaluate the string-field predicate family (eq / neq / in / notIn /
/// contains / hasPrefix / hasSuffix) against a column value. `None` predicates
/// are skipped (AND semantics, matching ent).
#[allow(clippy::too_many_arguments)]
fn str_field_matches(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    contains: &Option<String>,
    has_prefix: &Option<String>,
    has_suffix: &Option<String>,
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
    true
}

/// Whether an [`ApiKeyRow`] satisfies an [`APIKeyWhereInput`] tree. `not`/`and`/
/// `or` recurse (empty `and` matches, empty `or` is ignored — ent semantics).
fn api_key_row_matches_where(row: &ApiKeyRow, w: &APIKeyWhereInput) -> bool {
    if let Some(inner) = &w.not
        && api_key_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| api_key_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| api_key_row_matches_where(row, c))
    {
        return false;
    }

    // project_id (GUID → numeric-string compare).
    if let Some(p) = &w.project_id
        && decode_gql_id(p.as_str()).as_deref() != Some(row.project_id.as_str())
    {
        return false;
    }
    if let Some(p) = &w.project_id_neq
        && decode_gql_id(p.as_str()).as_deref() == Some(row.project_id.as_str())
    {
        return false;
    }
    if let Some(list) = &w.project_id_in
        && !list
            .iter()
            .any(|p| decode_gql_id(p.as_str()).as_deref() == Some(row.project_id.as_str()))
    {
        return false;
    }
    if let Some(list) = &w.project_id_not_in
        && list
            .iter()
            .any(|p| decode_gql_id(p.as_str()).as_deref() == Some(row.project_id.as_str()))
    {
        return false;
    }

    // user_id.
    if let Some(u) = &w.user_id
        && row.user_id.as_deref() != decode_gql_id(u.as_str()).as_deref()
    {
        return false;
    }
    if w.user_id_is_nil == Some(true) && row.user_id.is_some() {
        return false;
    }
    if w.user_id_not_nil == Some(true) && row.user_id.is_none() {
        return false;
    }

    // status enum predicates.
    if let Some(s) = w.status
        && row.status != status_to_wire(s)
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row.status == status_to_wire(s)
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list.iter().any(|s| row.status == status_to_wire(*s))
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list.iter().any(|s| row.status == status_to_wire(*s))
    {
        return false;
    }

    // type enum predicates.
    if let Some(t) = w.key_type
        && row.key_type != key_type_to_wire(t)
    {
        return false;
    }
    if let Some(t) = w.type_neq
        && row.key_type == key_type_to_wire(t)
    {
        return false;
    }
    if let Some(list) = &w.type_in
        && !list.iter().any(|t| row.key_type == key_type_to_wire(*t))
    {
        return false;
    }
    if let Some(list) = &w.type_not_in
        && list.iter().any(|t| row.key_type == key_type_to_wire(*t))
    {
        return false;
    }

    // name / key string families.
    if !str_field_matches(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
    ) {
        return false;
    }
    if !str_field_matches(
        &row.key,
        &w.key,
        &w.key_neq,
        &w.key_in,
        &w.key_not_in,
        &w.key_contains,
        &w.key_has_prefix,
        &w.key_has_suffix,
    ) {
        return false;
    }

    true
}

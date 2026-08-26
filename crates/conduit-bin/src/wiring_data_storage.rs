//! ADPT-DATA-STORAGE — host adapter wiring the admin GraphQL DataStorage
//! domain to the configured [`DataStorageRepo`].
//!
//! Backs the host-injected traits declared in
//! `crates/conduit-admin-graphql/src/data_storage.rs`:
//!   - [`DataStorageQueryServices`]    — `Query.dataStorages` connection.
//!   - [`DataStorageMutationServices`] — `createDataStorage` /
//!     `updateDataStorage`.
//!   - [`DataStorageServices`] (unified marker) — satisfied automatically via
//!     the crate's blanket impl once both halves above are implemented; the
//!     host registers a single `Arc<dyn DataStorageServices>`.
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//!   - `Query.dataStorages` (`internal/server/gql/ent.resolvers.go` →
//!     ent `Paginate`): the crate layer already lowered `orderBy`
//!     (`CREATED_AT` → ent default ID order) into
//!     [`DataStorageOrderSelection`]; the repo returns rows in
//!     `created_at ASC, id ASC` order, so we re-sort per the selection and
//!     apply Relay offset pagination in-memory (the data_storages table is
//!     tiny — same bounded-materialization strategy `ChannelCrudAdapter`
//!     uses in `wiring_channel_crud.rs`).
//!   - `createDataStorage` → `biz.DataStorageService.CreateDataStorage`
//!     (`biz/data_storage.go:192`): duplicate-name check, then create with
//!     `primary=false` and `status=active` **unconditionally** — the
//!     GraphQL input's optional `status` is IGNORED by the Go service (it
//!     calls `SetStatus(datastorage.StatusActive)` regardless). The repo's
//!     INSERT also hardcodes `status='active'`, matching.
//!   - `updateDataStorage` → `biz.DataStorageService.UpdateDataStorage`
//!     (`biz/data_storage.go:224`): read existing → duplicate-name check
//!     excluding self → `mergeSettings` (`biz/data_storage.go:707`) →
//!     partial update of name/description/status/settings. `type` and
//!     `primary` are immutable post-create (no `type` field on the update
//!     input; `primary` is `Immutable()` in the ent schema).
//!   - Settings merge sensitivity (`biz/data_storage.go:707-809`):
//!     "Sensitive fields (credentials) are only updated if explicitly
//!     provided" — S3 `accessKey`/`secretKey`, GCS `credential` and WebDAV
//!     `password` are kept from the existing row when the input leaves them
//!     empty. A sub-config counts as "provided" per Go's `isS3Provided` /
//!     `isGCSProvided` / `isWebDAVProvided` (any non-credential field
//!     non-empty also counts; S3 `pathStyle` alone does NOT).
//!   - Settings JSON column shape mirrors Go `objects.DataStorageSettings`
//!     (`internal/objects/data_stograge.go`): keys `dsn` / `directory` /
//!     `s3{bucketName,endpoint,region,accessKey,secretKey,pathStyle}` /
//!     `gcs{bucketName,credential}` /
//!     `webdav{url,username,password,insecure_skip_tls,path}` — note the
//!     snake_case `insecure_skip_tls` tag. No `omitempty` on any tag, so we
//!     serialize every key (nulls included) exactly like Go's marshaller.
//!   - The GraphQL *output* `S3` type has NO `accessKey`/`secretKey` fields
//!     and the output `GCS` type has NO `credential` field (they are
//!     input-only, sensitive) — the row→GraphQL conversion drops them.
//!
//! ## Not implemented here (and why — no `DEFER` stubs were needed)
//! The two crate traits only declare `dataStorages` / `createDataStorage` /
//! `updateDataStorage`, all of which the repo fully supports, so every trait
//! method below is real DB-backed behavior. Operations that exist in the Go
//! codebase but are NOT part of this crate slice (per the data_storage.rs
//! module doc): `deleteDataStorage` (not in the Go schema snapshot),
//! `updateDefaultDataStorage` (system-settings slice) and any live
//! test-connection probe against S3/GCS/WebDAV (no GraphQL operation for it
//! in the snapshot). They belong to other slices/hosts and must not be
//! invented here.
//!
//! ## `where` predicate coverage (Query.dataStorages)
//! Covered in-memory: `not`/`and`/`or` recursion plus every scalar-column
//! predicate family the crate declares — `id`, `createdAt`, `updatedAt`,
//! `name`, `description`, `primary`, `type`, `status`. Edge predicates
//! (`hasRequestsWith`/`hasExecutionsWith`) are not declared by the crate's
//! `DataStorageWhereInput` (pending other slices), so there is nothing to
//! evaluate for them.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use conduit_admin_graphql::data_storage::{
    CreateDataStorageInput, DataStorage as GqlDataStorage, DataStorageConnection,
    DataStorageConnectionArgs, DataStorageEdge, DataStorageMutationServices, DataStorageOrderTerm,
    DataStorageQueryServices, DataStorageServiceError,
    DataStorageSettings as GqlDataStorageSettings, DataStorageSettingsInput, DataStorageStatus,
    DataStorageType, DataStorageWhereInput, GCS as GqlGcs, GCSInput, OrderDirection, S3 as GqlS3,
    S3Input, UpdateDataStorageInput, WebDAV as GqlWebDav, WebDAVInput,
};
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_db::repo::data_storage_repo::{
    CreateDataStorageInput as RepoCreateDataStorageInput, ListDataStoragesQuery,
    UpdateDataStorageInput as RepoUpdateDataStorageInput,
};
use conduit_db::row::DataStorageRow;
use conduit_db::{DataStorageRepo, PolicyContext, Principal, RepoError, RequestContext};

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing DataStorage adapter backed by the live
/// [`DataStorageRepo`]. Implements both [`DataStorageQueryServices`] and
/// [`DataStorageMutationServices`] (and therefore the unified
/// [`conduit_admin_graphql::data_storage::DataStorageServices`] via the
/// crate's blanket impl).
pub struct DataStorageAdapter {
    repo: Arc<dyn DataStorageRepo>,
}

impl DataStorageAdapter {
    pub fn new(repo: Arc<dyn DataStorageRepo>) -> Self {
        Self { repo }
    }

    /// Materialize every live (non-deleted) data-storage row, paging through
    /// the repo in generous windows. The data_storages table is tiny (a
    /// handful of rows), so a full in-memory load mirrors Go's ent
    /// `.Paginate(...)` over the whole table faithfully.
    async fn load_all(&self) -> Result<Vec<DataStorageRow>, DataStorageServiceError> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListDataStoragesQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
            };
            let result = self
                .repo
                .list_data_storages_unchecked(&ctx, &query)
                .await
                .map_err(|e| DataStorageServiceError::Query(e.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
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

/// Decode a GraphQL `ID!` (`gid://conduit/DataStorage/<n>` wire form or a
/// bare numeric id) into the numeric DB-id string the repo expects. Mirrors
/// Go `GUID.UnmarshalGQL`; a value that is neither is treated as "no such
/// row" (same convention as `wiring_channel_crud::channel_db_id`).
fn data_storage_db_id(raw: &str) -> Option<String> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

/// Decode a filter-side GraphQL `ID` into the numeric DB id for predicate
/// comparison. Undecodable values simply never match (ent would reject them
/// at unmarshal time; we degrade to an empty match instead of erroring the
/// whole connection).
fn id_from_gql(id: &async_graphql::ID) -> Option<i64> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(id.as_str()) {
        return Some(guid.id);
    }
    id.as_str().parse::<i64>().ok()
}

// ---------------------------------------------------------------------------
// Enum ↔ wire-literal maps (the `type` / `status` columns store the Go ent
// enum literals verbatim).
// ---------------------------------------------------------------------------

/// GraphQL `DataStorageType` → the lowercase wire literal stored in the
/// `"type"` column (Go `ent/datastorage.Type`: database|fs|s3|gcs|webdav).
fn storage_type_to_wire(t: DataStorageType) -> &'static str {
    match t {
        DataStorageType::Database => "database",
        DataStorageType::Fs => "fs",
        DataStorageType::S3 => "s3",
        DataStorageType::Gcs => "gcs",
        DataStorageType::Webdav => "webdav",
    }
}

/// Wire literal → GraphQL `DataStorageType`. An unknown literal degrades to
/// `database` (the ent column default) rather than failing the whole
/// connection — the enum is DB-constrained so this only fires on corrupt rows.
fn storage_type_from_wire(s: &str) -> DataStorageType {
    match s {
        "fs" => DataStorageType::Fs,
        "s3" => DataStorageType::S3,
        "gcs" => DataStorageType::Gcs,
        "webdav" => DataStorageType::Webdav,
        _ => DataStorageType::Database,
    }
}

/// GraphQL `DataStorageStatus` → the wire literal stored in the `status`
/// column (Go `ent/datastorage.Status`: active|archived).
fn status_to_wire(s: DataStorageStatus) -> &'static str {
    match s {
        DataStorageStatus::Active => "active",
        DataStorageStatus::Archived => "archived",
    }
}

/// Wire literal → GraphQL `DataStorageStatus`. Unknown degrades to `active`
/// (the ent column default).
fn status_from_wire(s: &str) -> DataStorageStatus {
    match s {
        "archived" => DataStorageStatus::Archived,
        _ => DataStorageStatus::Active,
    }
}

// ---------------------------------------------------------------------------
// Settings JSON column ↔ typed wire shape.
//
// Mirrors Go `objects.DataStorageSettings` / `objects.S3` / `objects.GCS` /
// `objects.WebDAV` (`conduit/internal/objects/data_stograge.go`) — the exact
// JSON the Go marshaller writes into the ent `settings` column. Go's struct
// tags carry no `omitempty`, so serialization emits every key (nil pointers
// as null), which these derives reproduce.
// ---------------------------------------------------------------------------

/// Go `objects.DataStorageSettings` — pointer fields → `Option`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct WireSettings {
    #[serde(default)]
    dsn: Option<String>,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    s3: Option<WireS3>,
    #[serde(default)]
    gcs: Option<WireGcs>,
    #[serde(default)]
    webdav: Option<WireWebDav>,
}

/// Go `objects.S3` — plain (non-pointer) string/bool fields, camelCase tags.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireS3 {
    #[serde(default)]
    bucket_name: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    access_key: String,
    #[serde(default)]
    secret_key: String,
    #[serde(default)]
    path_style: bool,
}

/// Go `objects.GCS` — camelCase `bucketName` + lowercase `credential`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGcs {
    #[serde(default)]
    bucket_name: String,
    #[serde(default)]
    credential: String,
}

/// Go `objects.WebDAV` — every tag is already lowercase/snake_case
/// (`url`/`username`/`password`/`insecure_skip_tls`/`path`), so the Rust
/// field names match verbatim without a rename.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct WireWebDav {
    #[serde(default)]
    url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    insecure_skip_tls: bool,
    #[serde(default)]
    path: String,
}

/// Parse the stored `settings` JSON column. A malformed/legacy value degrades
/// to the all-empty settings rather than failing the row (mirrors Go, where a
/// nil `Settings` pointer is tolerated everywhere).
fn settings_from_row(value: &Value) -> WireSettings {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Serialize the wire settings back into the JSON column value.
fn wire_settings_to_json(settings: &WireSettings) -> Value {
    serde_json::to_value(settings).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

// ---------------------------------------------------------------------------
// GraphQL input → wire settings (the Go resolver lowers `*Input` types onto
// `objects.*` with zero-value defaults for absent optionals).
// ---------------------------------------------------------------------------

fn s3_input_to_wire(input: S3Input) -> WireS3 {
    WireS3 {
        bucket_name: input.bucket_name,
        endpoint: input.endpoint.unwrap_or_default(),
        region: input.region.unwrap_or_default(),
        access_key: input.access_key.unwrap_or_default(),
        secret_key: input.secret_key.unwrap_or_default(),
        path_style: input.path_style.unwrap_or(false),
    }
}

fn gcs_input_to_wire(input: GCSInput) -> WireGcs {
    WireGcs {
        bucket_name: input.bucket_name,
        credential: input.credential.unwrap_or_default(),
    }
}

fn webdav_input_to_wire(input: WebDAVInput) -> WireWebDav {
    WireWebDav {
        url: input.url,
        username: input.username.unwrap_or_default(),
        password: input.password.unwrap_or_default(),
        insecure_skip_tls: input.insecure_skip_tls.unwrap_or(false),
        path: input.path.unwrap_or_default(),
    }
}

fn settings_input_to_wire(input: DataStorageSettingsInput) -> WireSettings {
    WireSettings {
        dsn: input.dsn,
        directory: input.directory,
        s3: input.s3.map(s3_input_to_wire),
        gcs: input.gcs.map(gcs_input_to_wire),
        webdav: input.webdav.map(webdav_input_to_wire),
    }
}

// ---------------------------------------------------------------------------
// Settings merge — Go `DataStorageService.mergeSettings`
// (`biz/data_storage.go:707-809`).
// ---------------------------------------------------------------------------

/// Go `isS3Provided` (`biz/data_storage.go:677`): any of the S3 fields except
/// `pathStyle` is non-empty.
fn is_s3_provided(s3: &WireS3) -> bool {
    !s3.bucket_name.is_empty()
        || !s3.endpoint.is_empty()
        || !s3.region.is_empty()
        || !s3.access_key.is_empty()
        || !s3.secret_key.is_empty()
}

/// Go `isGCSProvided` (`biz/data_storage.go:696`).
fn is_gcs_provided(gcs: &WireGcs) -> bool {
    !gcs.bucket_name.is_empty() || !gcs.credential.is_empty()
}

/// Go `isWebDAVProvided` (`biz/data_storage.go:803`): any of url / username /
/// password / path non-empty (`insecure_skip_tls` alone does NOT count).
fn is_webdav_provided(webdav: &WireWebDav) -> bool {
    !webdav.url.is_empty()
        || !webdav.username.is_empty()
        || !webdav.password.is_empty()
        || !webdav.path.is_empty()
}

/// Merge existing and new settings, preserving sensitive fields that the
/// input leaves empty (Go `mergeSettings`, `biz/data_storage.go:707-809`):
///   - `directory` / `dsn`: input value wins when provided, else existing.
///   - S3: if the input S3 is "provided", non-sensitive fields come from the
///     input verbatim while `accessKey`/`secretKey` fall back to the existing
///     values when the input leaves them empty; otherwise the existing S3
///     block is preserved as-is.
///   - GCS: same pattern with `credential` as the sensitive field.
///   - WebDAV: same pattern with `password` as the sensitive field.
fn merge_settings(existing: &WireSettings, input: &WireSettings) -> WireSettings {
    // Directory (non-sensitive) then DSN (sensitive for database storages,
    // but Go merges it with plain provided-wins semantics).
    let directory = input
        .directory
        .clone()
        .or_else(|| existing.directory.clone());
    let dsn = input.dsn.clone().or_else(|| existing.dsn.clone());

    // S3.
    let s3 = match &input.s3 {
        Some(s3_in) if is_s3_provided(s3_in) => Some(WireS3 {
            bucket_name: s3_in.bucket_name.clone(),
            endpoint: s3_in.endpoint.clone(),
            region: s3_in.region.clone(),
            path_style: s3_in.path_style,
            access_key: if s3_in.access_key.is_empty() {
                existing
                    .s3
                    .as_ref()
                    .map(|e| e.access_key.clone())
                    .unwrap_or_default()
            } else {
                s3_in.access_key.clone()
            },
            secret_key: if s3_in.secret_key.is_empty() {
                existing
                    .s3
                    .as_ref()
                    .map(|e| e.secret_key.clone())
                    .unwrap_or_default()
            } else {
                s3_in.secret_key.clone()
            },
        }),
        // Input S3 absent or all-empty → preserve the existing block.
        _ => existing.s3.clone(),
    };

    // GCS.
    let gcs = match &input.gcs {
        Some(gcs_in) if is_gcs_provided(gcs_in) => Some(WireGcs {
            bucket_name: gcs_in.bucket_name.clone(),
            credential: if gcs_in.credential.is_empty() {
                existing
                    .gcs
                    .as_ref()
                    .map(|e| e.credential.clone())
                    .unwrap_or_default()
            } else {
                gcs_in.credential.clone()
            },
        }),
        _ => existing.gcs.clone(),
    };

    // WebDAV.
    let webdav = match &input.webdav {
        Some(w_in) if is_webdav_provided(w_in) => Some(WireWebDav {
            url: w_in.url.clone(),
            username: w_in.username.clone(),
            insecure_skip_tls: w_in.insecure_skip_tls,
            path: w_in.path.clone(),
            password: if w_in.password.is_empty() {
                existing
                    .webdav
                    .as_ref()
                    .map(|e| e.password.clone())
                    .unwrap_or_default()
            } else {
                w_in.password.clone()
            },
        }),
        _ => existing.webdav.clone(),
    };

    WireSettings {
        dsn,
        directory,
        s3,
        gcs,
        webdav,
    }
}

// ---------------------------------------------------------------------------
// Row → GraphQL conversion.
// ---------------------------------------------------------------------------

/// Wire settings → the GraphQL output object. Sensitive fields are dropped
/// where the snapshot output type omits them (S3 `accessKey`/`secretKey`,
/// GCS `credential` — input-only fields). WebDAV values are surfaced
/// verbatim, mirroring Go gqlgen which resolves the plain-string model
/// fields directly (empty string, not null, for unset values).
fn wire_settings_to_gql(settings: WireSettings) -> GqlDataStorageSettings {
    GqlDataStorageSettings {
        dsn: settings.dsn,
        directory: settings.directory,
        s3: settings.s3.map(|s3| GqlS3 {
            bucket_name: s3.bucket_name,
            endpoint: s3.endpoint,
            region: s3.region,
            path_style: s3.path_style,
        }),
        gcs: settings.gcs.map(|gcs| GqlGcs {
            bucket_name: gcs.bucket_name,
        }),
        webdav: settings.webdav.map(|w| GqlWebDav {
            url: w.url,
            username: Some(w.username),
            password: Some(w.password),
            insecure_skip_tls: Some(w.insecure_skip_tls),
            path: Some(w.path),
        }),
    }
}

/// `DataStorageRow` → GraphQL `DataStorage`. The id uses the ent global-id
/// wire form (`gid://conduit/DataStorage/<n>`), matching every other node
/// type this host emits.
fn data_storage_row_to_gql(row: DataStorageRow) -> GqlDataStorage {
    let wire = settings_from_row(&row.settings);
    GqlDataStorage {
        id: format!("gid://conduit/DataStorage/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        description: row.description,
        primary: row.primary,
        storage_type: storage_type_from_wire(&row.storage_type),
        settings: wire_settings_to_gql(wire),
        status: status_from_wire(&row.status),
    }
}

// ---------------------------------------------------------------------------
// DataStorageQueryServices — Query.dataStorages
// ---------------------------------------------------------------------------

#[async_trait]
impl DataStorageQueryServices for DataStorageAdapter {
    async fn data_storages(
        &self,
        args: DataStorageConnectionArgs,
    ) -> Result<DataStorageConnection, DataStorageServiceError> {
        let rows = self.load_all().await?;

        // `where` filter (in-memory; see module doc for covered families).
        let mut rows: Vec<DataStorageRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Ordering: the crate already lowered `CREATED_AT` → `Id` (ent
        // default DataStorage order). The repo returns created_at-asc order,
        // so re-sort for any explicit selection.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    DataStorageOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    DataStorageOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let nodes: Vec<GqlDataStorage> = rows.into_iter().map(data_storage_row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme (matching
        // `connection_from_offset_page`; `before`/`last` backward paging is
        // not requested by the admin UI — same as the channel adapter). A
        // malformed `after` degrades to offset 0 rather than failing the
        // whole query.
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

        Ok(DataStorageConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(DataStorageEdge {
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
// DataStorageMutationServices — create / update
// ---------------------------------------------------------------------------

#[async_trait]
impl DataStorageMutationServices for DataStorageAdapter {
    async fn create_data_storage(
        &self,
        input: CreateDataStorageInput,
    ) -> Result<GqlDataStorage, DataStorageServiceError> {
        let ctx = boot_request_context();
        // Retain the name for the duplicate-name error (the repo's
        // `NameConflict` maps to Go `xerrors.DuplicateNameError("data
        // storage", …)`).
        let name = input.name.clone();
        let wire = settings_input_to_wire(input.settings);

        let repo_input = RepoCreateDataStorageInput {
            // PostgreSQL owns the generated PK; `id` is ignored
            // on insert (read-back uses the DB id).
            id: String::new(),
            name: input.name,
            description: input.description,
            // Go biz create: SetPrimary(false) — the public API never
            // creates a primary storage (only the system database is).
            primary: false,
            storage_type: Some(storage_type_to_wire(input.storage_type).to_string()),
            settings: Some(wire_settings_to_json(&wire)),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        // Go biz create IGNORES the optional `input.status` and forces
        // `active` (`SetStatus(datastorage.StatusActive)`,
        // biz/data_storage.go:211); the repo INSERT hardcodes 'active' too.
        let _ = input.status;

        let row = self
            .repo
            .create_data_storage_unchecked(&ctx, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => DataStorageServiceError::DuplicateName(name),
                other => DataStorageServiceError::Create(other.to_string()),
            })?;
        Ok(data_storage_row_to_gql(row))
    }

    async fn update_data_storage(
        &self,
        id: &str,
        input: UpdateDataStorageInput,
    ) -> Result<GqlDataStorage, DataStorageServiceError> {
        let ctx = boot_request_context();
        // Undecodable / unknown id → ent's not-found error string (mirrors
        // Go: `DataStorage.Get` fails before anything else runs).
        let db_id = data_storage_db_id(id).ok_or(DataStorageServiceError::NotFound)?;

        // Go reads the existing row first — required for the settings merge.
        let existing = self
            .repo
            .find_data_storage_unchecked(&ctx, &db_id)
            .await
            .map_err(|e| DataStorageServiceError::Update(e.to_string()))?
            .ok_or(DataStorageServiceError::NotFound)?;

        // Name (if any) retained for the duplicate-name error.
        let name = input.name.clone();

        // Settings: only touched when the input provides them (Go:
        // `if input.Settings != nil { mutation.SetSettings(merged) }`).
        let settings = input.settings.map(|settings_input| {
            let existing_wire = settings_from_row(&existing.settings);
            let input_wire = settings_input_to_wire(settings_input);
            wire_settings_to_json(&merge_settings(&existing_wire, &input_wire))
        });

        let repo_input = RepoUpdateDataStorageInput {
            name: input.name,
            description: input.description,
            // `type` is immutable post-create (no `type` field on the
            // GraphQL update input; snapshot lines 7401-7420).
            storage_type: None,
            settings,
            status: input.status.map(|s| status_to_wire(s).to_string()),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let row = self
            .repo
            .update_data_storage_unchecked(&ctx, &db_id, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => {
                    DataStorageServiceError::DuplicateName(name.unwrap_or_default())
                }
                RepoError::NotFound(_) => DataStorageServiceError::NotFound,
                other => DataStorageServiceError::Update(other.to_string()),
            })?;
        Ok(data_storage_row_to_gql(row))
    }
}

// ---------------------------------------------------------------------------
// `where` predicate evaluation (Query.dataStorages)
// ---------------------------------------------------------------------------

/// Whether a `DataStorageRow` satisfies a `DataStorageWhereInput` predicate
/// tree. `not`/`and`/`or` recurse; an empty `and` matches (ent semantics)
/// and an empty `or` is ignored so it never blacks out the whole result.
fn row_matches_where(row: &DataStorageRow, w: &DataStorageWhereInput) -> bool {
    if let Some(inner) = &w.not
        && row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| row_matches_where(row, c))
    {
        return false;
    }

    // id numeric family (filter IDs accept gid or bare numeric form).
    let row_id = row.id.parse::<i64>().unwrap_or(i64::MAX);
    if !id_family(
        row_id,
        &w.id,
        &w.id_neq,
        &w.id_in,
        &w.id_not_in,
        &w.id_gt,
        &w.id_gte,
        &w.id_lt,
        &w.id_lte,
    ) {
        return false;
    }

    // created_at / updated_at time families.
    if !dt_family(
        row.created_at,
        &w.created_at,
        &w.created_at_neq,
        &w.created_at_in,
        &w.created_at_not_in,
        &w.created_at_gt,
        &w.created_at_gte,
        &w.created_at_lt,
        &w.created_at_lte,
    ) {
        return false;
    }
    if !dt_family(
        row.updated_at,
        &w.updated_at,
        &w.updated_at_neq,
        &w.updated_at_in,
        &w.updated_at_not_in,
        &w.updated_at_gt,
        &w.updated_at_gte,
        &w.updated_at_lt,
        &w.updated_at_lte,
    ) {
        return false;
    }

    // name string family.
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

    // description string family.
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

    // primary boolean predicates.
    if let Some(v) = w.primary
        && row.primary != v
    {
        return false;
    }
    if let Some(v) = w.primary_neq
        && row.primary == v
    {
        return false;
    }

    // type enum predicates.
    if let Some(t) = w.storage_type
        && row.storage_type != storage_type_to_wire(t)
    {
        return false;
    }
    if let Some(t) = w.type_neq
        && row.storage_type == storage_type_to_wire(t)
    {
        return false;
    }
    if let Some(list) = &w.type_in
        && !list
            .iter()
            .any(|t| row.storage_type == storage_type_to_wire(*t))
    {
        return false;
    }
    if let Some(list) = &w.type_not_in
        && list
            .iter()
            .any(|t| row.storage_type == storage_type_to_wire(*t))
    {
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

    true
}

/// Evaluate the id numeric-predicate family. Filter IDs that cannot be
/// decoded never match (see `id_from_gql`). `None` predicates are skipped
/// (AND semantics, matching ent).
#[allow(clippy::too_many_arguments)]
fn id_family(
    row_id: i64,
    eq: &Option<async_graphql::ID>,
    neq: &Option<async_graphql::ID>,
    in_set: &Option<Vec<async_graphql::ID>>,
    not_in: &Option<Vec<async_graphql::ID>>,
    gt: &Option<async_graphql::ID>,
    gte: &Option<async_graphql::ID>,
    lt: &Option<async_graphql::ID>,
    lte: &Option<async_graphql::ID>,
) -> bool {
    if let Some(v) = eq
        && id_from_gql(v) != Some(row_id)
    {
        return false;
    }
    if let Some(v) = neq
        && id_from_gql(v) == Some(row_id)
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().any(|v| id_from_gql(v) == Some(row_id))
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|v| id_from_gql(v) == Some(row_id))
    {
        return false;
    }
    if let Some(v) = gt {
        match id_from_gql(v) {
            Some(x) if row_id > x => {}
            _ => return false,
        }
    }
    if let Some(v) = gte {
        match id_from_gql(v) {
            Some(x) if row_id >= x => {}
            _ => return false,
        }
    }
    if let Some(v) = lt {
        match id_from_gql(v) {
            Some(x) if row_id < x => {}
            _ => return false,
        }
    }
    if let Some(v) = lte {
        match id_from_gql(v) {
            Some(x) if row_id <= x => {}
            _ => return false,
        }
    }
    true
}

/// Evaluate a timestamp-predicate family against a column value. `None`
/// predicates are skipped (AND semantics, matching ent).
#[allow(clippy::too_many_arguments)]
fn dt_family(
    value: chrono::DateTime<chrono::Utc>,
    eq: &Option<TimeScalar>,
    neq: &Option<TimeScalar>,
    in_set: &Option<Vec<TimeScalar>>,
    not_in: &Option<Vec<TimeScalar>>,
    gt: &Option<TimeScalar>,
    gte: &Option<TimeScalar>,
    lt: &Option<TimeScalar>,
    lte: &Option<TimeScalar>,
) -> bool {
    if let Some(v) = eq
        && value != v.0
    {
        return false;
    }
    if let Some(v) = neq
        && value == v.0
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().any(|v| v.0 == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|v| v.0 == value)
    {
        return false;
    }
    if let Some(v) = gt
        && value <= v.0
    {
        return false;
    }
    if let Some(v) = gte
        && value < v.0
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v.0
    {
        return false;
    }
    if let Some(v) = lte
        && value > v.0
    {
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
        && (value <= v.as_str())
    {
        return false;
    }
    if let Some(v) = gte
        && (value < v.as_str())
    {
        return false;
    }
    if let Some(v) = lt
        && (value >= v.as_str())
    {
        return false;
    }
    if let Some(v) = lte
        && (value > v.as_str())
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

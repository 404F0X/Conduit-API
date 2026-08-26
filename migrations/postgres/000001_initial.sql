-- Conduit API initial PostgreSQL migration.  -- RUST-P3-002 S04/S09/S13
--
-- Materializes the 24-table Go Ent schema snapshot using PostgreSQL-native
-- types. The Go Ent schema is the source of truth for table, column, and index
-- names.
--
-- Dialect mapping (applied uniformly across all 24 tables):
--   * `INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL` (Ent auto key)
--       -> `BIGSERIAL` (i.e. BIGINT identity with sequence; S11 keeps the
--          integer-domain id so existing client code is unchanged).
--   * `INTEGER` (Ent field.Int / field.Int64 edge columns, bool flags)
--       -> `BIGINT`. Bool flags (is_owner, stream, content_saved, ready,
--          auto_sync_supported_models, pass_through_applied, "primary") use
--          native `BOOLEAN` instead (see S09/S13: prefer native postgres
--          types where they exist).
--   * `TEXT` (plain strings, enums) -> `TEXT`.
--   * Enum columns stay `TEXT` — no native `CREATE TYPE` enums in phase 1
--     (RUST-P3-002 S14: enums remain strings at the storage boundary).
--   * `DATETIME` (TimeMixin created_at/updated_at; all timestamp fields)
--       -> `TIMESTAMPTZ NOT NULL DEFAULT now()`. Nullable timestamp fields
--          (Optional+Nillable) become plain `TIMESTAMPTZ` (no DEFAULT).
--   * JSON fields (field.JSON / field.Strings) `TEXT` -> `JSONB` (S09/S13:
--          prefer JSONB on postgres for indexability + validation). Defaults
--          are JSON literals: `'[]'::jsonb`, `'{}'::jsonb`, or struct defaults.
--   * Decimal-as-JSON (`channel_model_prices.price`,
--          `channel_model_price_versions.price`) -> `JSONB` (it is a
--          `field.JSON(objects.ModelPrice{})`, NOT a numeric scalar; the inner
--          decimal items round-trip via rust_decimal/serde).
--   * `REAL` (usage_logs.total_cost, a true Go float64, NOT decimal)
--       -> `DOUBLE PRECISION`.
--   * `deleted_at INTEGER NOT NULL DEFAULT 0` (SoftDeleteMixin, Go field.Int)
--       -> `BIGINT NOT NULL DEFAULT 0`. Kept as BIGINT (NOT BOOLEAN) on
--          purpose: the Go SoftDeleteMixin stores a unix timestamp when
--          soft-deleted and 0 otherwise; preserving that representation keeps
--          the application contract stable (S11).
--   * No FOREIGN KEY constraints (Go `WithForeignKeys(false)`); edges are
--     plain BIGINT columns. S06.
--   * Index names, table names, column names, and column order follow the
--     source schema contract.
--
-- Source of truth: Go Ent schema under
--   conduit/internal/ent/schema/*.go + mixins in
--   conduit/internal/ent/schema/mixin.go (TimeMixin)
--   conduit/internal/ent/schema/schematype/soft_delete.go (SoftDeleteMixin)

-- ---------------------------------------------------------------------------
-- Table: systems  (System entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS systems (
    -- Ent auto key -> BIGSERIAL.
    id              BIGSERIAL PRIMARY KEY,

    -- System.key: unique string identifier for the setting.
    key             TEXT NOT NULL,

    -- System.value: free-form setting value. TEXT accommodates large values
    -- such as base64-encoded logo images.
    value           TEXT NOT NULL,

    -- TimeMixin.created_at: immutable, server default now().
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- TimeMixin.updated_at: default now(), refreshed on update by app code.
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- SoftDeleteMixin.deleted_at: BIGINT (NOT BOOLEAN — see header). 0 == not
    -- deleted, unix timestamp == soft-deleted.
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

-- Implicit unique constraint from System.key (field.String("key").Unique()).
CREATE UNIQUE INDEX systems_key_key ON systems (key);

-- ---------------------------------------------------------------------------
-- Table: threads  (Thread entity)
-- Mixins: TimeMixin (no soft-delete)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS threads (
    id              BIGSERIAL PRIMARY KEY,

    -- Thread.project_id: immutable int referencing projects.id (no FK).
    project_id      BIGINT NOT NULL,

    -- Thread.thread_id: unique string identifier.
    thread_id       TEXT NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX threads_by_project_id ON threads (project_id);
CREATE UNIQUE INDEX threads_by_thread_id ON threads (thread_id);

-- ---------------------------------------------------------------------------
-- Table: traces  (Trace entity)
-- Mixins: TimeMixin (no soft-delete)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS traces (
    id              BIGSERIAL PRIMARY KEY,

    project_id      BIGINT NOT NULL,
    trace_id        TEXT NOT NULL,

    -- Trace.thread_id: Optional()+Immutable() => nullable BIGINT.
    thread_id       BIGINT,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX traces_by_project_id ON traces (project_id);
CREATE UNIQUE INDEX traces_by_trace_id ON traces (trace_id);
CREATE INDEX traces_by_thread_id ON traces (thread_id);

-- ===========================================================================
-- Identity entities
-- ===========================================================================

-- ---------------------------------------------------------------------------
-- Table: roles  (Role entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS roles (
    id              BIGSERIAL PRIMARY KEY,

    name            TEXT NOT NULL,

    -- Role.level: enum {system, project}, default "system", immutable.
    -- Stored as TEXT (S14: no native enum type in phase 1).
    level           TEXT NOT NULL DEFAULT 'system',

    -- Role.project_id: Optional()+Nillable() => nullable BIGINT.
    project_id      BIGINT,

    -- Role.scopes: field.Strings => JSON array. JSONB; default empty array.
    scopes          JSONB NOT NULL DEFAULT '[]'::jsonb,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX roles_by_project_id_name ON roles (project_id, name);
CREATE INDEX roles_by_level ON roles (level);

-- ---------------------------------------------------------------------------
-- Table: users  (User entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS users (
    id              BIGSERIAL PRIMARY KEY,

    email           TEXT NOT NULL,

    -- User.status: enum {activated, deactivated}, default "activated".
    status          TEXT NOT NULL DEFAULT 'activated',

    prefer_language TEXT NOT NULL DEFAULT 'en',

    -- User.password: Sensitive() string (hash).
    password        TEXT NOT NULL,

    first_name      TEXT NOT NULL DEFAULT '',
    last_name       TEXT NOT NULL DEFAULT '',

    -- User.avatar: Optional() => nullable TEXT (mediumtext in Go).
    avatar          TEXT,

    -- User.is_owner: bool -> native BOOLEAN.
    is_owner        BOOLEAN NOT NULL DEFAULT FALSE,

    -- User.scopes: field.Strings => JSONB array.
    scopes          JSONB NOT NULL DEFAULT '[]'::jsonb,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX users_email_deleted_at ON users (email, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: projects  (Project entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS projects (
    id              BIGSERIAL PRIMARY KEY,

    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',

    -- Project.status: enum {active, archived}, default "active".
    status          TEXT NOT NULL DEFAULT 'active',

    -- Project.profiles: field.JSON(&objects.ProjectProfiles{}), Optional().
    -- JSONB, nullable.
    profiles        JSONB,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX projects_by_name ON projects (name, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: user_roles  (UserRole entity)
-- Mixins: NONE (custom Optional+Nillable timestamps; no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS user_roles (
    id              BIGSERIAL PRIMARY KEY,

    user_id         BIGINT NOT NULL,
    role_id         BIGINT NOT NULL,

    -- UserRole custom timestamps: Optional()+Nillable() => NULLABLE.
    -- No DEFAULT (compat with old data).
    created_at      TIMESTAMPTZ,
    updated_at      TIMESTAMPTZ
);

CREATE UNIQUE INDEX user_roles_by_user_id_role_id ON user_roles (user_id, role_id);
CREATE INDEX user_roles_by_role_id ON user_roles (role_id);

-- ---------------------------------------------------------------------------
-- Table: user_projects  (UserProject entity)
-- Mixin: TimeMixin only (NO SoftDeleteMixin => no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS user_projects (
    id              BIGSERIAL PRIMARY KEY,

    user_id         BIGINT NOT NULL,
    project_id      BIGINT NOT NULL,

    -- UserProject.is_owner: bool default false, MUTABLE -> BOOLEAN.
    is_owner        BOOLEAN NOT NULL DEFAULT FALSE,

    -- UserProject.scopes: field.Strings => JSONB array.
    scopes          JSONB NOT NULL DEFAULT '[]'::jsonb,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX user_projects_by_user_id_project_id
    ON user_projects (user_id, project_id);
CREATE INDEX user_projects_by_project_id ON user_projects (project_id);

-- ===========================================================================
-- Medium entities
-- ===========================================================================

-- ---------------------------------------------------------------------------
-- Table: api_keys  (APIKey entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_keys (
    id              BIGSERIAL PRIMARY KEY,

    -- APIKey.user_id: Optional()+Immutable() => nullable BIGINT.
    user_id         BIGINT,

    -- APIKey.project_id: Immutable(), Default(1).
    project_id      BIGINT NOT NULL DEFAULT 1,

    key             TEXT NOT NULL,
    name            TEXT NOT NULL,

    -- APIKey.type: enum {user, service_account, noauth}, default "user".
    "type"          TEXT NOT NULL DEFAULT 'user',

    -- APIKey.status: enum {enabled, disabled, archived}, default "enabled".
    status          TEXT NOT NULL DEFAULT 'enabled',

    -- APIKey.scopes: field.Strings => JSONB array, Optional => nullable.
    scopes          JSONB,

    -- APIKey.profiles: field.JSON(&objects.APIKeyProfiles{}), Optional.
    profiles        JSONB,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX api_keys_by_user_id ON api_keys (user_id);
CREATE INDEX api_keys_by_project_id ON api_keys (project_id);
CREATE UNIQUE INDEX api_keys_by_key ON api_keys (key);

-- ---------------------------------------------------------------------------
-- Table: models  (Model entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS models (
    id              BIGSERIAL PRIMARY KEY,

    developer       TEXT NOT NULL,
    model_id        TEXT NOT NULL,

    -- Model.type: enum (chat/embedding/rerank/image_generation/video_generation),
    -- default "chat".
    "type"          TEXT NOT NULL DEFAULT 'chat',

    name            TEXT NOT NULL,
    icon            TEXT NOT NULL,

    -- "group" is a SQL reserved word; quoted for portability.
    "group"         TEXT NOT NULL,

    -- Model.model_card / settings: field.JSON => JSONB NOT NULL.
    model_card      JSONB NOT NULL,
    settings        JSONB NOT NULL,

    -- Model.status: enum {enabled, disabled, archived}, default "disabled".
    status          TEXT NOT NULL DEFAULT 'disabled',

    -- Model.remark: Optional()+Nillable() => nullable TEXT.
    remark          TEXT,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX models_by_name ON models (name, deleted_at);
CREATE UNIQUE INDEX models_by_model_id ON models (model_id, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: oidc_identities  (OIDCIdentity entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS oidc_identities (
    id              BIGSERIAL PRIMARY KEY,

    issuer          TEXT NOT NULL,
    subject         TEXT NOT NULL,

    -- OIDCIdentity.email / idp_name: Optional() => nullable TEXT.
    email           TEXT,
    idp_name        TEXT,

    -- last_login_at: Optional()+Nillable() => nullable TIMESTAMPTZ.
    last_login_at   TIMESTAMPTZ,

    -- OIDCIdentity.user_id: int, Required edge => NOT NULL.
    user_id         BIGINT NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX oidc_identities_by_issuer_subject_deleted_at
    ON oidc_identities (issuer, subject, deleted_at);
CREATE INDEX oidc_identities_by_user_id ON oidc_identities (user_id);

-- ---------------------------------------------------------------------------
-- Table: prompts  (Prompt entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS prompts (
    id              BIGSERIAL PRIMARY KEY,

    -- Prompt.project_id: Immutable() int (no FK).
    project_id      BIGINT NOT NULL,

    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    role            TEXT NOT NULL,

    -- Prompt.content: plain string.
    content         TEXT NOT NULL,

    -- Prompt.status: enum {enabled, disabled}, default "disabled".
    status          TEXT NOT NULL DEFAULT 'disabled',

    -- "order" is a SQL reserved word; quoted.
    "order"         BIGINT NOT NULL DEFAULT 0,

    -- Prompt.settings: field.JSON => JSONB NOT NULL.
    settings        JSONB NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX prompts_by_project_id ON prompts (project_id);
CREATE UNIQUE INDEX prompts_by_project_id_name
    ON prompts (project_id, name, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: prompt_protection_rules  (PromptProtectionRule entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS prompt_protection_rules (
    id              BIGSERIAL PRIMARY KEY,

    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    pattern         TEXT NOT NULL,

    -- PromptProtectionRule.status: enum {enabled, disabled, archived}.
    status          TEXT NOT NULL DEFAULT 'disabled',

    -- settings: field.JSON => JSONB NOT NULL.
    settings        JSONB NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX prompt_protection_rules_by_name
    ON prompt_protection_rules (name, deleted_at);

-- ===========================================================================
-- Channel/provisioning entities
-- ===========================================================================

-- ---------------------------------------------------------------------------
-- Table: channel_probes  (ChannelProbe entity)
-- Mixins: NONE
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS channel_probes (
    id              BIGSERIAL PRIMARY KEY,

    channel_id      BIGINT NOT NULL,
    total_request_count     BIGINT NOT NULL,
    success_request_count   BIGINT NOT NULL,

    -- avg_tokens_per_second / avg_time_to_first_token_ms: Go float64,
    -- Optional+Nillable => nullable DOUBLE PRECISION.
    avg_tokens_per_second   DOUBLE PRECISION,
    avg_time_to_first_token_ms DOUBLE PRECISION,

    -- ChannelProbe.timestamp: int64, immutable => BIGINT.
    timestamp       BIGINT NOT NULL
);

CREATE INDEX channel_probes_by_channel_id_timestamp
    ON channel_probes (channel_id, timestamp);

-- ---------------------------------------------------------------------------
-- Table: data_storages  (DataStorage entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS data_storages (
    id              BIGSERIAL PRIMARY KEY,

    name            TEXT NOT NULL,
    description     TEXT NOT NULL,

    -- DataStorage.primary: bool, immutable -> BOOLEAN. Quoted SQL keyword.
    "primary"       BOOLEAN NOT NULL DEFAULT FALSE,

    -- DataStorage.type: enum {database, fs, s3, gcs, webdav}, immutable.
    "type"          TEXT NOT NULL,

    -- settings: field.JSON => JSONB NOT NULL.
    settings        JSONB NOT NULL,

    -- status: enum {active, archived}, default "active".
    status          TEXT NOT NULL DEFAULT 'active',

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

-- Legacy StorageKey name "data_sources_by_name".
CREATE UNIQUE INDEX data_sources_by_name ON data_storages (name);

-- ---------------------------------------------------------------------------
-- Table: provider_quota_status  (ProviderQuotaStatus entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS provider_quota_status (
    id              BIGSERIAL PRIMARY KEY,

    channel_id      BIGINT NOT NULL,

    -- provider_type: enum immutable => TEXT.
    provider_type   TEXT NOT NULL,

    -- status: enum {available, warning, exhausted, unknown} => TEXT.
    status          TEXT NOT NULL,

    -- quota_data: field.JSON(map[string]any{}) => JSONB NOT NULL.
    quota_data      JSONB NOT NULL,

    -- next_reset_at: Optional+Nillable => nullable TIMESTAMPTZ.
    next_reset_at   TIMESTAMPTZ,

    -- ready: bool, default true -> BOOLEAN.
    ready           BOOLEAN NOT NULL DEFAULT TRUE,

    -- next_check_at: time, NOT NULL (no Optional).
    next_check_at   TIMESTAMPTZ NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX provider_quota_status_channel_id
    ON provider_quota_status (channel_id);
CREATE INDEX provider_quota_status_next_check_at
    ON provider_quota_status (next_check_at);

-- ---------------------------------------------------------------------------
-- Table: channel_override_templates  (ChannelOverrideTemplate entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS channel_override_templates (
    id              BIGSERIAL PRIMARY KEY,

    -- user_id: Optional()+Immutable() => nullable BIGINT.
    user_id         BIGINT,

    name            TEXT NOT NULL,

    -- description: Optional() => nullable TEXT.
    description     TEXT,

    -- override_parameters: string default "{}". Deprecated; kept as TEXT
    -- (it is a plain string, not a typed JSON object in Go).
    override_parameters TEXT NOT NULL DEFAULT '{}',

    -- override_headers: field.JSON([]HeaderEntry{}) => JSONB NOT NULL default [].
    override_headers JSONB NOT NULL DEFAULT '[]'::jsonb,

    -- header/body_override_operations: Optional => nullable JSONB.
    header_override_operations JSONB,
    body_override_operations   JSONB,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX channel_override_templates_by_user_name
    ON channel_override_templates (user_id, name, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: channel_model_prices  (ChannelModelPrice entity)
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS channel_model_prices (
    id              BIGSERIAL PRIMARY KEY,

    channel_id      BIGINT NOT NULL,
    model_id        TEXT NOT NULL,

    -- price: field.JSON(objects.ModelPrice{}) => JSONB NOT NULL. Carries
    -- decimal price items round-tripped via rust_decimal/serde (S13: JSONB
    -- preserves numeric precision better than TEXT for JSON numbers).
    price           JSONB NOT NULL,

    reference_id    TEXT NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX channel_model_prices_by_channel_id_model_id
    ON channel_model_prices (channel_id, model_id, deleted_at);
CREATE UNIQUE INDEX channel_model_prices_reference_id
    ON channel_model_prices (reference_id);

-- ---------------------------------------------------------------------------
-- Table: channel_model_price_versions  (ChannelModelPriceVersion entity)
-- Mixin: TimeMixin only (NO SoftDeleteMixin => no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS channel_model_price_versions (
    id              BIGSERIAL PRIMARY KEY,

    channel_id      BIGINT NOT NULL,
    model_id        TEXT NOT NULL,
    channel_model_price_id BIGINT NOT NULL,

    -- price: field.JSON(objects.ModelPrice{}), immutable => JSONB NOT NULL.
    price           JSONB NOT NULL,

    -- status: enum {active, archived} => TEXT.
    status          TEXT NOT NULL,

    effective_start_at TIMESTAMPTZ NOT NULL,

    -- effective_end_at: Optional+Nillable => nullable TIMESTAMPTZ.
    effective_end_at   TIMESTAMPTZ,

    reference_id    TEXT NOT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX channel_model_price_versions_reference_id
    ON channel_model_price_versions (reference_id);

-- ---------------------------------------------------------------------------
-- Table: api_key_profile_templates  (APIKeyProfileTemplate entity)
--                                                  -- RUST-P3-002 DATA-02
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS api_key_profile_templates (
    id              BIGSERIAL PRIMARY KEY,

    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',

    -- APIKeyProfileTemplate.project_id: Immutable() int (Required edge, no FK).
    project_id      BIGINT NOT NULL,

    -- profile: field.JSON(&objects.APIKeyProfile{}), Optional+Default =>
    -- nullable JSONB.
    profile         JSONB,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX api_key_profile_templates_by_project_name
    ON api_key_profile_templates (project_id, name, deleted_at);

-- ---------------------------------------------------------------------------
-- Table: channels  (Channel entity)  -- RUST-P3-002 DATA-03
-- Mixins: TimeMixin + SoftDeleteMixin
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS channels (
    id              BIGSERIAL PRIMARY KEY,

    -- Channel.type: enum ~60 provider values => TEXT.
    "type"          TEXT NOT NULL,

    -- base_url: Optional() => nullable TEXT.
    base_url        TEXT,

    name            TEXT NOT NULL,

    -- status: enum {enabled, disabled, archived}, default "disabled".
    status          TEXT NOT NULL DEFAULT 'disabled',

    -- credentials: field.JSON(ChannelCredentials{}), Sensitive(), no Optional
    -- => JSONB NOT NULL.
    credentials     JSONB NOT NULL,

    -- disabled_api_keys: field.JSON([]DisabledAPIKey{}), Optional => nullable
    -- JSONB.
    disabled_api_keys JSONB,

    -- supported_models: field.Strings, no Optional => JSONB NOT NULL default [].
    supported_models JSONB NOT NULL DEFAULT '[]'::jsonb,

    -- manual_models / tags: field.Strings, Optional => nullable JSONB.
    manual_models   JSONB,
    tags            JSONB,

    -- auto_sync_supported_models: bool -> BOOLEAN.
    auto_sync_supported_models BOOLEAN NOT NULL DEFAULT FALSE,

    -- auto_sync_model_pattern: Optional+Default("") => nullable TEXT.
    auto_sync_model_pattern TEXT,

    default_test_model TEXT NOT NULL,

    -- policies / settings / endpoints: field.JSON, Optional => nullable JSONB.
    policies        JSONB,
    settings        JSONB,
    endpoints       JSONB,

    -- ordering_weight: int, default 0.
    ordering_weight BIGINT NOT NULL DEFAULT 0,

    -- error_message / remark: Optional+Nillable => nullable TEXT.
    error_message   TEXT,
    remark          TEXT,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at      BIGINT NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX channels_by_name ON channels (name, deleted_at);

-- ===========================================================================
-- Large entities
-- ===========================================================================

-- ---------------------------------------------------------------------------
-- Table: requests  (Request entity)  -- RUST-P3-002 DATA-15
-- Mixin: TimeMixin ONLY (NO SoftDeleteMixin => no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS requests (
    id              BIGSERIAL PRIMARY KEY,

    -- api_key_id: Optional+Immutable => nullable BIGINT.
    api_key_id      BIGINT,

    -- project_id: Immutable+Default(1) => NOT NULL BIGINT.
    project_id      BIGINT NOT NULL DEFAULT 1,

    -- trace_id / data_storage_id: Optional+Immutable => nullable BIGINT.
    trace_id        BIGINT,
    data_storage_id BIGINT,

    -- source: enum {api, playground, test}, default "api", immutable => TEXT.
    "source"        TEXT NOT NULL DEFAULT 'api',

    model_id        TEXT NOT NULL,

    -- reasoning_effort: Optional+Immutable => nullable TEXT.
    reasoning_effort TEXT,

    -- format: Immutable+Default => NOT NULL TEXT.
    format          TEXT NOT NULL DEFAULT 'openai/chat_completions',

    -- request_headers / response_body / response_chunks: Optional => nullable
    -- JSONB (objects.JSONRawMessage = []byte raw JSON).
    request_headers JSONB,

    -- request_body: Immutable, no Optional => JSONB NOT NULL.
    request_body    JSONB NOT NULL,

    response_body   JSONB,
    response_chunks JSONB,

    -- channel_id: Optional => nullable BIGINT.
    channel_id      BIGINT,

    -- external_id: Optional+MaxLen(512) => nullable TEXT.
    external_id     TEXT,

    -- status: enum {pending, processing, completed, failed, canceled}, no
    -- default => NOT NULL TEXT.
    "status"        TEXT NOT NULL,

    -- stream: bool -> BOOLEAN.
    stream          BOOLEAN NOT NULL DEFAULT FALSE,

    -- client_ip: default "", immutable => TEXT.
    client_ip       TEXT NOT NULL DEFAULT '',

    -- metrics_*: Int64, Optional+Nillable => nullable BIGINT.
    metrics_latency_ms BIGINT,
    metrics_first_token_latency_ms BIGINT,
    metrics_reasoning_duration_ms BIGINT,

    -- content_saved: bool -> BOOLEAN.
    content_saved   BOOLEAN NOT NULL DEFAULT FALSE,

    content_storage_id BIGINT,
    content_storage_key TEXT,
    content_saved_at TIMESTAMPTZ,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX requests_by_api_key_id_created_at ON requests (api_key_id, created_at);
CREATE INDEX requests_by_project_id_created_at ON requests (project_id, created_at);
CREATE INDEX requests_by_channel_id_created_at ON requests (channel_id, created_at);
CREATE INDEX requests_by_trace_id_created_at ON requests (trace_id, created_at);
CREATE INDEX requests_by_created_at ON requests (created_at);

-- ---------------------------------------------------------------------------
-- Table: request_executions  (RequestExecution entity)  -- RUST-P3-002 DATA-16
-- Mixin: TimeMixin ONLY (NO SoftDeleteMixin => no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS request_executions (
    id              BIGSERIAL PRIMARY KEY,

    project_id      BIGINT NOT NULL DEFAULT 1,
    request_id      BIGINT NOT NULL,

    -- channel_id / data_storage_id: Optional+Immutable => nullable BIGINT.
    channel_id      BIGINT,
    data_storage_id BIGINT,

    external_id     TEXT,
    model_id        TEXT NOT NULL,
    format          TEXT NOT NULL DEFAULT 'openai/chat_completions',

    -- request_body: Immutable, no Optional => JSONB NOT NULL.
    request_body    JSONB NOT NULL,

    -- response_body / response_chunks / request_headers: Optional => nullable
    -- JSONB.
    response_body   JSONB,
    response_chunks JSONB,
    request_headers JSONB,

    -- error_message: Optional => nullable TEXT.
    error_message   TEXT,

    -- response_status_code: Int, Optional+Nillable => nullable BIGINT.
    response_status_code BIGINT,

    -- status: enum, no default => NOT NULL TEXT.
    "status"        TEXT NOT NULL,

    -- stream: bool -> BOOLEAN.
    stream          BOOLEAN NOT NULL DEFAULT FALSE,

    -- metrics_*: Int64, Optional+Nillable => nullable BIGINT.
    metrics_latency_ms BIGINT,
    metrics_first_token_latency_ms BIGINT,
    metrics_reasoning_duration_ms BIGINT,

    -- request_url: Optional => nullable TEXT.
    request_url     TEXT,

    -- pass_through_applied: bool -> BOOLEAN.
    pass_through_applied BOOLEAN NOT NULL DEFAULT FALSE,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX request_executions_by_request_id_status_created_at
    ON request_executions (request_id, status, created_at);
CREATE INDEX request_executions_by_request_id_created_at
    ON request_executions (request_id, created_at);
CREATE INDEX request_executions_by_channel_id_created_at
    ON request_executions (channel_id, created_at);

-- ---------------------------------------------------------------------------
-- Table: usage_logs  (UsageLog entity)  -- RUST-P3-002 DATA-21
-- Mixin: TimeMixin ONLY (NO SoftDeleteMixin => no deleted_at)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS usage_logs (
    id              BIGSERIAL PRIMARY KEY,

    -- request_id: Int, Immutable (Required edge) => NOT NULL BIGINT.
    request_id      BIGINT NOT NULL,

    -- api_key_id / channel_id: Optional+Immutable => nullable BIGINT.
    api_key_id      BIGINT,
    channel_id      BIGINT,

    -- project_id: Immutable+Default(1) => NOT NULL BIGINT.
    project_id      BIGINT NOT NULL DEFAULT 1,

    -- model_id: String, Immutable => NOT NULL TEXT.
    model_id        TEXT NOT NULL,

    -- Core token metrics: Int64, Default(0), NO Optional => NOT NULL BIGINT.
    prompt_tokens       BIGINT NOT NULL DEFAULT 0,
    completion_tokens   BIGINT NOT NULL DEFAULT 0,
    total_tokens        BIGINT NOT NULL DEFAULT 0,

    -- Detailed token breakdown: Int64, Optional+Default(0) => nullable BIGINT.
    prompt_audio_tokens             BIGINT,
    prompt_cached_tokens            BIGINT,
    prompt_write_cached_tokens      BIGINT,
    prompt_write_cached_tokens_5m   BIGINT,
    prompt_write_cached_tokens_1h   BIGINT,
    completion_audio_tokens             BIGINT,
    completion_reasoning_tokens         BIGINT,
    completion_accepted_prediction_tokens BIGINT,
    completion_rejected_prediction_tokens BIGINT,

    -- source: enum {api, playground, test}, default "api" => TEXT.
    "source"        TEXT NOT NULL DEFAULT 'api',

    format          TEXT NOT NULL DEFAULT 'openai/chat_completions',

    -- total_cost: field.Float, Nillable+Optional => nullable DOUBLE PRECISION.
    -- TRUE Go float64 (NOT decimal/JSONB).
    total_cost      DOUBLE PRECISION,

    -- cost_items: field.JSON([]CostItem{}), Optional => nullable JSONB.
    cost_items      JSONB,

    -- cost_price_reference_id: Optional => nullable TEXT.
    cost_price_reference_id TEXT,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX usage_logs_by_request_id ON usage_logs (request_id);
CREATE INDEX usage_logs_by_created_at ON usage_logs (created_at);
CREATE INDEX usage_logs_by_model_id_created_at ON usage_logs (model_id, created_at);
CREATE INDEX usage_logs_by_project_id_created_at ON usage_logs (project_id, created_at);
CREATE INDEX usage_logs_by_channel_id_created_at ON usage_logs (channel_id, created_at);
CREATE INDEX usage_logs_by_api_key_id_created_at ON usage_logs (api_key_id, created_at);

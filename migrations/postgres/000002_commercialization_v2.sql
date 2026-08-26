-- Conduit API commercialization v2 (additive only).


CREATE TABLE IF NOT EXISTS upstream_model_deployments (
    id BIGSERIAL PRIMARY KEY,
    channel_id BIGINT NOT NULL,
    upstream_model_id TEXT NOT NULL,
    internal_name TEXT NOT NULL,
    variant TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'enabled',
    source TEXT NOT NULL DEFAULT 'discovered',
    procurement_price TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX upstream_model_deployments_identity ON upstream_model_deployments (channel_id, upstream_model_id, variant);
CREATE INDEX upstream_model_deployments_by_channel ON upstream_model_deployments (channel_id, status);

CREATE TABLE IF NOT EXISTS model_routes (
    id BIGSERIAL PRIMARY KEY,
    public_model_id BIGINT NOT NULL,
    deployment_id BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX model_routes_identity ON model_routes (public_model_id, deployment_id);
CREATE INDEX model_routes_by_deployment ON model_routes (deployment_id, status);


CREATE TABLE IF NOT EXISTS price_books (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    currency TEXT NOT NULL DEFAULT 'CNY',
    status TEXT NOT NULL DEFAULT 'enabled',
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS price_book_versions (
    id BIGSERIAL PRIMARY KEY,
    price_book_id BIGINT NOT NULL,
    version BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'published',
    reference_id TEXT NOT NULL UNIQUE,
    effective_start_at TIMESTAMPTZ,
    effective_end_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX price_book_versions_number ON price_book_versions (price_book_id, version);
CREATE INDEX price_book_versions_active ON price_book_versions (price_book_id, status, effective_start_at);
ALTER TABLE price_book_versions
    ADD CONSTRAINT price_book_versions_status_known
    CHECK (status IN ('published', 'archived'));
CREATE TABLE IF NOT EXISTS price_book_items (
    id BIGSERIAL PRIMARY KEY,
    price_book_version_id BIGINT NOT NULL,
    public_model_id BIGINT NOT NULL,
    price JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX price_book_items_identity ON price_book_items (price_book_version_id, public_model_id);


CREATE TABLE IF NOT EXISTS customer_charge_events (
    id BIGSERIAL PRIMARY KEY,
    usage_log_id BIGINT NOT NULL UNIQUE,
    request_id BIGINT NOT NULL,
    public_model_id BIGINT,
    price_book_version_id BIGINT,
    amount NUMERIC(30, 12),
    -- Charge amounts are converted from the retail/accounting currency into
    -- the stable station-credit ledger unit before this event is written.
    currency TEXT NOT NULL DEFAULT 'STATION_CREDIT',
    applied_rules_snapshot JSONB NOT NULL DEFAULT '[]'::jsonb,
    usage_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    calculation_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX customer_charge_events_by_request ON customer_charge_events (request_id, created_at);

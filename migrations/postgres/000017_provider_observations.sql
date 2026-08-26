-- Append-only upstream observations; never authoritative runtime prices.

CREATE TABLE IF NOT EXISTS provider_quota_observations (
    id BIGSERIAL PRIMARY KEY, channel_id BIGINT NOT NULL, provider_type TEXT NOT NULL,
    probe_adapter TEXT, status TEXT NOT NULL, success BOOLEAN NOT NULL,
    currency TEXT, total TEXT, used TEXT, remaining TEXT, unlimited BOOLEAN,
    balance_source TEXT, quota_data JSONB, error_message TEXT,
    observed_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS provider_quota_observations_channel_time
    ON provider_quota_observations (channel_id, observed_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS provider_quota_observations_status_time
    ON provider_quota_observations (success, observed_at DESC);

CREATE TABLE IF NOT EXISTS provider_price_snapshots (
    id BIGSERIAL PRIMARY KEY, channel_id BIGINT NOT NULL, adapter_id TEXT NOT NULL,
    adapter_version TEXT NOT NULL, primary_endpoint TEXT, attempted_endpoints JSONB NOT NULL,
    pricing_version TEXT, raw_payload_sha256 TEXT, status TEXT NOT NULL,
    error_message TEXT, warnings JSONB NOT NULL, started_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS provider_price_snapshots_channel_time
    ON provider_price_snapshots (channel_id, observed_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS provider_price_rows (
    id BIGSERIAL PRIMARY KEY, snapshot_id BIGINT NOT NULL, channel_id BIGINT NOT NULL,
    upstream_model_id TEXT NOT NULL, group_name TEXT NOT NULL DEFAULT '',
    billing_kind TEXT NOT NULL, quality TEXT NOT NULL, currency TEXT,
    group_ratio TEXT, input_per_million TEXT, output_per_million TEXT,
    cache_read_per_million TEXT, cache_write_per_million TEXT, flat_per_request TEXT,
    reason TEXT, raw_item_sha256 TEXT,
    UNIQUE (snapshot_id, upstream_model_id, group_name, billing_kind)
);
CREATE INDEX IF NOT EXISTS provider_price_rows_channel_model
    ON provider_price_rows (channel_id, upstream_model_id, group_name);

CREATE TABLE IF NOT EXISTS provider_price_change_events (
    id BIGSERIAL PRIMARY KEY, channel_id BIGINT NOT NULL, from_snapshot_id BIGINT,
    to_snapshot_id BIGINT NOT NULL, upstream_model_id TEXT NOT NULL,
    group_name TEXT NOT NULL DEFAULT '', billing_kind TEXT NOT NULL,
    event_type TEXT NOT NULL, field_name TEXT, from_value TEXT, to_value TEXT,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS provider_price_change_events_channel_time
    ON provider_price_change_events (channel_id, created_at DESC, id DESC);

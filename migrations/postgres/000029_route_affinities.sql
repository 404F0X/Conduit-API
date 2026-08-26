-- Durable routing feedback for explicit provider continuity/cache keys.
-- Raw prompt-cache keys, response ids, prompts, and credentials are never stored.
CREATE TABLE IF NOT EXISTS route_affinities (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL,
    key_class TEXT NOT NULL,
    key_hash TEXT NOT NULL,
    public_model_id TEXT NOT NULL,
    api_format TEXT NOT NULL,
    channel_id BIGINT NOT NULL,
    upstream_model_id TEXT NOT NULL,
    upstream_api_format TEXT NOT NULL,
    credential_identity TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT route_affinities_key_class
        CHECK (key_class IN ('previous_response_id', 'prompt_cache_key')),
    CONSTRAINT route_affinities_key_hash
        CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT route_affinities_nonempty_scope
        CHECK (
            public_model_id <> '' AND api_format <> '' AND
            upstream_model_id <> '' AND upstream_api_format <> ''
        ),
    CONSTRAINT route_affinities_scope_unique
        UNIQUE (project_id, key_class, key_hash, public_model_id, api_format)
);

CREATE INDEX IF NOT EXISTS route_affinities_expires_at
    ON route_affinities (expires_at);

CREATE INDEX IF NOT EXISTS route_affinities_channel_id
    ON route_affinities (channel_id);

ALTER TABLE request_route_explanations
    ADD COLUMN IF NOT EXISTS affinity_key_class TEXT;

ALTER TABLE request_route_explanations
    ADD COLUMN IF NOT EXISTS affinity_decision TEXT;

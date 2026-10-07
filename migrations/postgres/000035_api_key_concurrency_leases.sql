CREATE TABLE api_key_concurrency_leases (
    lease_id UUID PRIMARY KEY,
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT api_key_concurrency_leases_request_unique
        UNIQUE (api_key_id, request_key)
);

CREATE INDEX api_key_concurrency_leases_key_expiry
    ON api_key_concurrency_leases (api_key_id, expires_at);

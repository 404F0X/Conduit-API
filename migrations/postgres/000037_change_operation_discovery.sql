-- One request identity per independent operation. Existing history is not regrouped.
ALTER TABLE change_sets ADD COLUMN operation_id UUID;
CREATE UNIQUE INDEX change_sets_operation_scope
    ON change_sets (operation_id, kind, scope_type, scope_id)
    WHERE operation_id IS NOT NULL;
ALTER TABLE provider_price_rows ADD COLUMN provenance JSONB NOT NULL DEFAULT '{}'::jsonb;

CREATE TABLE channel_operation_receipts (
    operation_id UUID PRIMARY KEY,
    channel_id BIGINT NOT NULL REFERENCES channels(id),
    operation_type TEXT NOT NULL CHECK (operation_type IN ('NEW_API_DISCOVERY', 'PROVIDER_PRICE_EDIT')),
    origin TEXT NOT NULL CHECK (origin IN ('CHANNEL_CREATE', 'USER_SYNC', 'AUTOMATIC_SYNC', 'MANUAL_EDIT', 'CHANNEL_IMPORT', 'CORRECTION')),
    created_by BIGINT,
    request_fingerprint TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    state TEXT NOT NULL CHECK (state IN ('NOT_STARTED', 'RUNNING', 'SUCCEEDED', 'PARTIAL', 'NEEDS_ACTION', 'FAILED', 'INTERRUPTED')),
    stage_results JSONB NOT NULL DEFAULT '{}'::jsonb,
    lease_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK ((state = 'RUNNING') = (lease_expires_at IS NOT NULL))
);
CREATE INDEX channel_operation_receipts_history
    ON channel_operation_receipts (channel_id, created_at DESC, operation_id);

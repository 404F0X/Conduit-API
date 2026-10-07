-- Durable deduplication survives usage-log retention. Receipts are never GC'd.
CREATE TABLE usage_recovery_receipts (
    event_key TEXT PRIMARY KEY,
    payload_hash TEXT NOT NULL,
    usage_log_id BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE maintenance_claims (
    job_key TEXT PRIMARY KEY,
    owner UUID NOT NULL,
    lease_until TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE artifact_deletion_queue (
    id BIGSERIAL PRIMARY KEY,
    data_storage_id BIGINT NOT NULL,
    object_key TEXT NOT NULL,
    delete_requested BOOLEAN NOT NULL DEFAULT TRUE,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(data_storage_id, object_key)
);

ALTER TABLE requests ADD COLUMN content_expired BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE requests ADD COLUMN activity_until TIMESTAMPTZ;
-- Retain the audit parent while accepted usage is awaiting journal handoff.
ALTER TABLE requests ADD COLUMN metering_pending BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE request_executions ADD COLUMN content_expired BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE request_executions ADD COLUMN activity_until TIMESTAMPTZ;
ALTER TABLE requests ADD COLUMN archive_next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE requests ADD COLUMN archive_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE requests ADD COLUMN archive_last_error TEXT;
CREATE INDEX requests_archive_due_idx ON requests(archive_next_attempt_at, id) WHERE content_saved = FALSE;

ALTER TABLE requests ADD COLUMN expired_artifacts TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[];
ALTER TABLE request_executions ADD COLUMN expired_artifacts TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[];

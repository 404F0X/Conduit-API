-- Append-only shadow reservation lifecycle and allocation audit. These rows
-- simulate Project settlement without mutating legacy or Project funds.

CREATE TABLE IF NOT EXISTS project_wallet_reservation_allocations (
    id BIGSERIAL PRIMARY KEY,
    reservation_id BIGINT NOT NULL,
    source_type TEXT NOT NULL,
    source_id BIGINT,
    amount_micros BIGINT NOT NULL CHECK (amount_micros >= 0),
    reserved_micros BIGINT NOT NULL CHECK (reserved_micros >= 0),
    captured_micros BIGINT NOT NULL DEFAULT 0 CHECK (captured_micros >= 0),
    released_micros BIGINT NOT NULL DEFAULT 0 CHECK (released_micros >= 0),
    allocation_class TEXT NOT NULL CHECK (allocation_class IN ('GENERAL','DEDICATED','PROJECT_CREDIT')),
    scope_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    expires_at_snapshot TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    CHECK (amount_micros = reserved_micros),
    CHECK (captured_micros + released_micros <= reserved_micros)
);
CREATE UNIQUE INDEX IF NOT EXISTS project_wallet_reservation_allocation_source
    ON project_wallet_reservation_allocations (reservation_id, source_type, source_id);
CREATE INDEX IF NOT EXISTS project_wallet_reservation_allocations_source
    ON project_wallet_reservation_allocations (source_type, source_id, reservation_id);
CREATE INDEX IF NOT EXISTS project_wallet_reservation_allocations_class_expiry
    ON project_wallet_reservation_allocations (reservation_id, allocation_class, expires_at_snapshot, id);

CREATE TABLE IF NOT EXISTS project_wallet_reservation_events (
    id BIGSERIAL PRIMARY KEY,
    reservation_id BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    amount_micros BIGINT NOT NULL,
    detail_snapshot TEXT NOT NULL DEFAULT '{}',
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS project_wallet_reservation_events_reservation
    ON project_wallet_reservation_events (reservation_id, id);

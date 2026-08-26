-- Independent Project wallet ledger for shadow-mode migration. Existing
-- user-scoped credit tables remain untouched until an explicit cutover.

CREATE TABLE IF NOT EXISTS project_wallets (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL,
    currency TEXT NOT NULL DEFAULT 'STATION_CREDIT',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS project_wallets_project_currency
    ON project_wallets (project_id, currency);

CREATE TABLE IF NOT EXISTS project_credit_ledger_entries (
    id BIGSERIAL PRIMARY KEY,
    wallet_id BIGINT NOT NULL,
    amount_micros BIGINT NOT NULL,
    entry_type TEXT NOT NULL,
    reference_type TEXT,
    reference_id TEXT,
    idempotency_key TEXT NOT NULL UNIQUE,
    description TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS project_credit_ledger_entries_wallet
    ON project_credit_ledger_entries (wallet_id, created_at, id);

CREATE TABLE IF NOT EXISTS project_wallet_reservations (
    id BIGSERIAL PRIMARY KEY,
    wallet_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    public_model_id BIGINT NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    amount_micros BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'shadow',
    expires_at TIMESTAMPTZ NOT NULL,
    settled_amount_micros BIGINT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS project_wallet_reservations_wallet_status
    ON project_wallet_reservations (wallet_id, status, expires_at);

CREATE TABLE IF NOT EXISTS credit_accounts (id BIGSERIAL PRIMARY KEY, user_id BIGINT NOT NULL, currency TEXT NOT NULL DEFAULT 'STATION_CREDIT', status TEXT NOT NULL DEFAULT 'enabled', created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS credit_accounts_user_currency ON credit_accounts (user_id, currency);
CREATE TABLE IF NOT EXISTS credit_ledger_entries (id BIGSERIAL PRIMARY KEY, account_id BIGINT NOT NULL, amount_micros BIGINT NOT NULL, entry_type TEXT NOT NULL, reference_type TEXT, reference_id TEXT, idempotency_key TEXT NOT NULL UNIQUE, description TEXT, metadata JSONB NOT NULL DEFAULT '{}'::jsonb, created_at TIMESTAMPTZ NOT NULL);
CREATE INDEX IF NOT EXISTS credit_ledger_entries_account ON credit_ledger_entries (account_id, created_at, id);
CREATE TABLE IF NOT EXISTS subscription_plans (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL UNIQUE, currency TEXT NOT NULL DEFAULT 'STATION_CREDIT' CHECK (currency='STATION_CREDIT'), interval_unit TEXT NOT NULL, interval_count INTEGER NOT NULL DEFAULT 1 CHECK (interval_count>0), status TEXT NOT NULL DEFAULT 'enabled', created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL);
CREATE TABLE IF NOT EXISTS user_subscriptions (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL,
    plan_id BIGINT NOT NULL,
    assignment_key TEXT NOT NULL,
    assignment_request_snapshot JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    current_period_start TIMESTAMPTZ NOT NULL,
    current_period_end TIMESTAMPTZ NOT NULL,
    assigned_interval_unit TEXT NOT NULL DEFAULT 'month'
        CHECK (assigned_interval_unit IN ('day','month','year')),
    assigned_interval_count INTEGER NOT NULL DEFAULT 1
        CHECK (assigned_interval_count > 0),
    auto_renew BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT user_subscriptions_user_fkey
        FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE RESTRICT,
    CONSTRAINT user_subscriptions_plan_fkey
        FOREIGN KEY(plan_id) REFERENCES subscription_plans(id) ON DELETE RESTRICT,
    CONSTRAINT user_subscriptions_assignment_key_key
        UNIQUE(assignment_key),
    CONSTRAINT user_subscriptions_assignment_key_normalized
        CHECK (assignment_key <> '' AND assignment_key = BTRIM(assignment_key)),
    CONSTRAINT user_subscriptions_assignment_request_object
        CHECK (jsonb_typeof(assignment_request_snapshot) = 'object'),
    CONSTRAINT user_subscriptions_period_positive
        CHECK (current_period_end > current_period_start)
);
CREATE INDEX IF NOT EXISTS user_subscriptions_user_status ON user_subscriptions (user_id, status, current_period_end);
CREATE TABLE IF NOT EXISTS credit_reservations (id BIGSERIAL PRIMARY KEY, account_id BIGINT NOT NULL, request_id BIGINT NOT NULL UNIQUE, amount_micros BIGINT NOT NULL, status TEXT NOT NULL DEFAULT 'reserved', expires_at TIMESTAMPTZ NOT NULL, settled_amount_micros BIGINT, created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL);
CREATE INDEX IF NOT EXISTS credit_reservations_account_status ON credit_reservations (account_id, status, expires_at);
CREATE TABLE IF NOT EXISTS charge_settlements (id BIGSERIAL PRIMARY KEY, charge_event_id BIGINT NOT NULL UNIQUE, user_id BIGINT, wallet_id BIGINT, amount_micros BIGINT NOT NULL, subscription_amount_micros BIGINT NOT NULL DEFAULT 0, credit_amount_micros BIGINT NOT NULL DEFAULT 0, status TEXT NOT NULL, detail_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb, created_at TIMESTAMPTZ NOT NULL);
CREATE INDEX IF NOT EXISTS charge_settlements_user ON charge_settlements (user_id, created_at);

-- Durable hand-off from persisted usage to asynchronous customer settlement.
CREATE TABLE IF NOT EXISTS usage_charge_outbox (
    usage_log_id BIGINT PRIMARY KEY,
    reservation_key TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS usage_charge_outbox_pending
    ON usage_charge_outbox (status, available_at, usage_log_id);

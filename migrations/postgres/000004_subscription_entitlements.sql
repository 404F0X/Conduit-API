-- Legacy duplicate removed; immutable terms live in 000006 snapshots.

/* CREATE TABLE IF NOT EXISTS user_subscription_plan_snapshots (
    subscription_id BIGINT PRIMARY KEY,
    plan_id BIGINT NOT NULL,
    plan_name TEXT NOT NULL,
    currency TEXT NOT NULL,
    allowance_micros BIGINT NOT NULL,
    interval_unit TEXT NOT NULL,
    interval_count INTEGER NOT NULL,
    rollover_mode TEXT NOT NULL,
    rollover_cap_micros BIGINT,
    plan_status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
); */

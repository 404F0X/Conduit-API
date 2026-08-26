-- Project-scoped subscription access grants.

CREATE TABLE IF NOT EXISTS subscription_plan_access_plans (
    subscription_plan_id BIGINT NOT NULL,
    access_plan_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (subscription_plan_id, access_plan_id)
);

CREATE TABLE IF NOT EXISTS user_subscription_projects (
    subscription_id BIGINT PRIMARY KEY,
    project_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS user_subscription_projects_project
    ON user_subscription_projects (project_id, subscription_id);

CREATE TABLE IF NOT EXISTS project_access_grants (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL,
    access_plan_version_id BIGINT NOT NULL,
    source_type TEXT NOT NULL,
    source_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    valid_from TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, source_type, source_id)
);
CREATE INDEX IF NOT EXISTS project_access_grants_active
    ON project_access_grants (project_id, status, valid_from, valid_until);

-- Periodic quota rules and immutable issuance snapshots. These are created
-- here (rather than in 000003) because dedicated rules reference access_plans.
CREATE TABLE IF NOT EXISTS subscription_quota_rules (
    id BIGSERIAL PRIMARY KEY,
    subscription_plan_id BIGINT NOT NULL REFERENCES subscription_plans(id) ON DELETE CASCADE,
    rule_key TEXT NOT NULL,
    name TEXT NOT NULL,
    quota_class TEXT NOT NULL CHECK (quota_class IN ('GENERAL','DEDICATED')),
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    rollover_mode TEXT NOT NULL DEFAULT 'none' CHECK (rollover_mode IN ('none','capped')),
    rollover_cap_micros BIGINT CHECK (rollover_cap_micros IS NULL OR rollover_cap_micros >= 0),
    carry_duration_seconds BIGINT CHECK (carry_duration_seconds IS NULL OR carry_duration_seconds > 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE(subscription_plan_id, rule_key),
    CHECK (rollover_mode='none' OR rollover_cap_micros IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS subscription_quota_rules_plan
    ON subscription_quota_rules(subscription_plan_id, quota_class, id);
CREATE TABLE IF NOT EXISTS subscription_quota_rule_access_plans (
    quota_rule_id BIGINT NOT NULL REFERENCES subscription_quota_rules(id) ON DELETE CASCADE,
    access_plan_id BIGINT NOT NULL REFERENCES access_plans(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (quota_rule_id, access_plan_id)
);
CREATE INDEX IF NOT EXISTS subscription_quota_rule_access_plans_plan
    ON subscription_quota_rule_access_plans(access_plan_id, quota_rule_id);

CREATE TABLE IF NOT EXISTS user_subscription_quota_rule_snapshots (
    id BIGSERIAL PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES user_subscriptions(id) ON DELETE CASCADE,
    rule_key TEXT NOT NULL, rule_name TEXT NOT NULL,
    quota_class TEXT NOT NULL CHECK (quota_class IN ('GENERAL','DEDICATED')),
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    rollover_mode TEXT NOT NULL CHECK (rollover_mode IN ('none','capped')),
    rollover_cap_micros BIGINT CHECK (rollover_cap_micros IS NULL OR rollover_cap_micros >= 0),
    carry_duration_seconds BIGINT CHECK (carry_duration_seconds IS NULL OR carry_duration_seconds > 0),
    access_plan_versions JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE(subscription_id, rule_key),
    CONSTRAINT user_subscription_quota_snapshot_owner_class_key
        UNIQUE(subscription_id, quota_class, id),
    CHECK (rollover_mode='none' OR rollover_cap_micros IS NOT NULL),
    CHECK ((quota_class='GENERAL' AND access_plan_versions='[]'::jsonb) OR
           (quota_class='DEDICATED' AND jsonb_array_length(access_plan_versions)>0))
);

CREATE TABLE IF NOT EXISTS subscription_entitlement_snapshots (
    id BIGSERIAL PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES user_subscriptions(id) ON DELETE CASCADE,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE(subscription_id, period_start),
    CONSTRAINT subscription_entitlement_snapshot_owner_id_key
        UNIQUE(subscription_id, id),
    CHECK(period_end > period_start)
);
CREATE TABLE IF NOT EXISTS subscription_entitlement_snapshot_items (
    id BIGSERIAL PRIMARY KEY,
    snapshot_id BIGINT NOT NULL REFERENCES subscription_entitlement_snapshots(id) ON DELETE CASCADE,
    quota_rule_snapshot_id BIGINT NOT NULL REFERENCES user_subscription_quota_rule_snapshots(id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    public_model_id BIGINT NOT NULL,
    UNIQUE(snapshot_id, quota_rule_snapshot_id, public_model_id)
);
CREATE INDEX IF NOT EXISTS subscription_entitlement_items_model
    ON subscription_entitlement_snapshot_items(public_model_id, snapshot_id);

CREATE TABLE IF NOT EXISTS subscription_allowance_buckets (
    id BIGSERIAL PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES user_subscriptions(id) ON DELETE CASCADE,
    quota_rule_id BIGINT REFERENCES subscription_quota_rules(id) ON DELETE SET NULL,
    quota_rule_snapshot_id BIGINT NOT NULL,
    entitlement_snapshot_id BIGINT NOT NULL,
    quota_class TEXT NOT NULL CHECK (quota_class IN ('GENERAL','DEDICATED')),
    scope_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    issued_at TIMESTAMPTZ NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    carryover_expires_at TIMESTAMPTZ,
    source_bucket_id BIGINT REFERENCES subscription_allowance_buckets(id) ON DELETE RESTRICT,
    granted_micros BIGINT NOT NULL CHECK (granted_micros >= 0),
    consumed_micros BIGINT NOT NULL DEFAULT 0 CHECK (consumed_micros >= 0),
    reserved_micros BIGINT NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    rollover_micros BIGINT NOT NULL DEFAULT 0 CHECK (rollover_micros >= 0),
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK(consumed_micros + reserved_micros <= granted_micros),
    CHECK(source_bucket_id IS NOT NULL OR expires_at >= period_end),
    CHECK(carryover_expires_at IS NULL OR carryover_expires_at >= expires_at),
    CHECK((source_bucket_id IS NULL AND rollover_micros=0) OR
          (source_bucket_id IS NOT NULL AND rollover_micros>0)),
    CONSTRAINT subscription_allowance_bucket_rule_owner_fkey
        FOREIGN KEY(subscription_id, quota_class, quota_rule_snapshot_id)
        REFERENCES user_subscription_quota_rule_snapshots(subscription_id, quota_class, id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT subscription_allowance_bucket_entitlement_owner_fkey
        FOREIGN KEY(subscription_id, entitlement_snapshot_id)
        REFERENCES subscription_entitlement_snapshots(subscription_id, id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX IF NOT EXISTS subscription_allowance_bucket_period_issue
    ON subscription_allowance_buckets(subscription_id, quota_rule_snapshot_id, period_start)
    WHERE source_bucket_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS subscription_allowance_bucket_carry_issue
    ON subscription_allowance_buckets(subscription_id, quota_rule_snapshot_id, period_start, source_bucket_id)
    WHERE source_bucket_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS subscription_allowance_bucket_expiry
    ON subscription_allowance_buckets(subscription_id, status, expires_at, quota_class, id);
CREATE INDEX IF NOT EXISTS subscription_allowance_bucket_scope
    ON subscription_allowance_buckets USING GIN(scope_snapshot);

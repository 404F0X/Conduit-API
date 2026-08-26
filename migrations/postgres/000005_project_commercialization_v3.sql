-- Project-scoped commercial policy. Additive bridge from the user/group v2 model.

CREATE TABLE IF NOT EXISTS access_plans (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'enabled',
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE IF NOT EXISTS access_plan_versions (
    id BIGSERIAL PRIMARY KEY,
    access_plan_id BIGINT NOT NULL,
    version INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft',
    reference_id TEXT NOT NULL UNIQUE,
    effective_start_at TIMESTAMPTZ,
    effective_end_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (access_plan_id, version)
);
CREATE INDEX IF NOT EXISTS access_plan_versions_active ON access_plan_versions (access_plan_id, status, effective_start_at);
CREATE TABLE IF NOT EXISTS access_plan_items (
    id BIGSERIAL PRIMARY KEY,
    access_plan_version_id BIGINT NOT NULL,
    public_model_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (access_plan_version_id, public_model_id)
);
CREATE TABLE IF NOT EXISTS access_plan_route_items (
    access_plan_version_id BIGINT NOT NULL,
    model_route_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (access_plan_version_id, model_route_id)
);
CREATE INDEX IF NOT EXISTS access_plan_route_items_route ON access_plan_route_items (model_route_id, access_plan_version_id);
CREATE TABLE IF NOT EXISTS price_tiers (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    multiplier_ppm BIGINT NOT NULL DEFAULT 1000000,
    status TEXT NOT NULL DEFAULT 'enabled',
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE IF NOT EXISTS project_commercial_profiles (
    project_id BIGINT PRIMARY KEY,
    account_type TEXT NOT NULL DEFAULT 'personal',
    base_access_plan_id BIGINT,
    base_price_tier_id BIGINT,
    -- Stable customer-ledger identity. The configurable display name (for
    -- example "神社塞钱") and real-world accounting currency live in system
    -- settings and must never be used as this key.
    billing_currency TEXT NOT NULL DEFAULT 'STATION_CREDIT',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE IF NOT EXISTS project_entitlement_overrides (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL,
    public_model_id BIGINT NOT NULL,
    effect TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source_id TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    valid_from TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS project_entitlement_overrides_lookup ON project_entitlement_overrides (project_id, public_model_id, status, valid_from, valid_until);
CREATE INDEX IF NOT EXISTS project_entitlement_overrides_source ON project_entitlement_overrides (source_type, source_id, status);
CREATE TABLE IF NOT EXISTS project_price_adjustments (
    id BIGSERIAL PRIMARY KEY,
    project_id BIGINT NOT NULL,
    multiplier_ppm BIGINT NOT NULL,
    stacking_key TEXT NOT NULL DEFAULT 'project-adjustment',
    priority INTEGER NOT NULL DEFAULT 0,
    source_type TEXT NOT NULL,
    source_id TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    valid_from TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS project_price_adjustments_lookup ON project_price_adjustments (project_id, status, stacking_key, priority);

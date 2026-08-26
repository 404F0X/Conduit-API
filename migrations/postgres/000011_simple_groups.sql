-- Simple-mode commercial bundle. This is a normalized facade over Project policy.

CREATE TABLE IF NOT EXISTS simple_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'enabled',
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    access_plan_id BIGINT NOT NULL REFERENCES access_plans(id),
    price_tier_id BIGINT NOT NULL REFERENCES price_tiers(id),
    default_subscription_plan_id BIGINT REFERENCES subscription_plans(id),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS simple_groups_commercial_bundle
    ON simple_groups (access_plan_id, price_tier_id, status);

CREATE TABLE IF NOT EXISTS simple_group_projects (
    simple_group_id TEXT NOT NULL REFERENCES simple_groups(id) ON DELETE CASCADE,
    project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (simple_group_id, project_id)
);
CREATE INDEX IF NOT EXISTS simple_group_projects_project
    ON simple_group_projects (project_id, simple_group_id);
CREATE UNIQUE INDEX IF NOT EXISTS simple_group_projects_one_group
    ON simple_group_projects (project_id);

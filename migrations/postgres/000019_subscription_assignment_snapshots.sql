-- Assignment-time model-group selection. The selected IDs stay fixed, while
-- renewable subscriptions follow each group's currently published version.

CREATE TABLE IF NOT EXISTS user_subscription_access_plan_snapshots (
    subscription_id BIGINT NOT NULL,
    access_plan_id BIGINT NOT NULL,
    access_plan_version_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (subscription_id, access_plan_id)
);
CREATE INDEX IF NOT EXISTS user_subscription_access_plan_snapshots_version
    ON user_subscription_access_plan_snapshots (access_plan_version_id, subscription_id);

CREATE TABLE IF NOT EXISTS api_key_quota_admissions (
    id BIGSERIAL PRIMARY KEY,
    api_key_id BIGINT NOT NULL,
    project_id BIGINT NOT NULL,
    profile_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS api_key_quota_admissions_key_time
    ON api_key_quota_admissions (api_key_id, profile_name, created_at, id);
CREATE INDEX IF NOT EXISTS api_key_quota_admissions_project_time
    ON api_key_quota_admissions (project_id, created_at, id);

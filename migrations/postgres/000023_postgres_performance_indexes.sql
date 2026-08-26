-- PostgreSQL access paths for the production request/operations workload.
-- Keep the time column last for equality-prefix lookups and time windows.
CREATE INDEX IF NOT EXISTS requests_by_project_created_at_desc
    ON requests (project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS requests_by_project_status_created_at_desc
    ON requests (project_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS request_executions_by_created_channel_model_credential
    ON request_executions (created_at, channel_id, model_id, credential_identity);
CREATE INDEX IF NOT EXISTS request_executions_by_request_channel_status_id_desc
    ON request_executions (request_id, channel_id, status, id DESC);
CREATE INDEX IF NOT EXISTS usage_logs_by_created_channel_request
    ON usage_logs (created_at, channel_id, request_id);
CREATE INDEX IF NOT EXISTS usage_logs_by_project_created_channel_model
    ON usage_logs (project_id, created_at, channel_id, model_id);

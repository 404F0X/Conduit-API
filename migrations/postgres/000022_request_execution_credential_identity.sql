ALTER TABLE request_executions
    ADD COLUMN credential_identity VARCHAR(80);

CREATE INDEX IF NOT EXISTS request_executions_by_channel_model_credential_created_at
    ON request_executions (channel_id, model_id, credential_identity, created_at);

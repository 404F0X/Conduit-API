-- Representative 100k-row plans prefer the narrower initial time/project
-- indexes for these aggregate shapes. The wider variants add write and vacuum
-- cost without a demonstrated access-path benefit.
DROP INDEX IF EXISTS usage_logs_by_created_channel_request;
DROP INDEX IF EXISTS usage_logs_by_project_created_channel_model;

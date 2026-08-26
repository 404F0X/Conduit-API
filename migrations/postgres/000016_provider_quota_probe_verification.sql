ALTER TABLE provider_quota_status ADD COLUMN probe_adapter TEXT;
ALTER TABLE provider_quota_status ADD COLUMN probe_verified_at TIMESTAMPTZ;

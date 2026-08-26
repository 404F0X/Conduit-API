-- Administrator-maintained provider website and billing quota snapshot.
-- Decimal values are stored exactly; they are not runtime quota limits.
ALTER TABLE channels ADD COLUMN website_url TEXT;
ALTER TABLE channels ADD COLUMN quota_currency VARCHAR(16);
ALTER TABLE channels ADD COLUMN actual_quota_used NUMERIC;
ALTER TABLE channels ADD COLUMN quota_remaining NUMERIC;

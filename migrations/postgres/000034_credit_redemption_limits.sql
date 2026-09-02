-- Allow a generated credit redemption code to be used a bounded number of
-- times while preserving one-time behavior for every batch created before
-- this migration. Existing rows receive max_redemptions = 1, so their state
-- and receipt history remain valid without a data rewrite.

ALTER TABLE credit_redemption_batches
    ADD COLUMN max_redemptions INTEGER NOT NULL DEFAULT 1
        CONSTRAINT credit_redemption_batches_max_redemptions_range
        CHECK (max_redemptions BETWEEN 1 AND 100000);

-- A retry by the same user for the original Project is idempotent. Different
-- users may redeem the same code until the batch limit is reached, while one
-- user cannot consume it again by switching Projects. code_id remains the
-- leading key so count-by-code stays indexed.
ALTER TABLE credit_redemption_receipts
    DROP CONSTRAINT credit_redemption_receipts_code_id_key;

ALTER TABLE credit_redemption_receipts
    ADD CONSTRAINT credit_redemption_receipts_code_user_unique
        UNIQUE (code_id, user_id);

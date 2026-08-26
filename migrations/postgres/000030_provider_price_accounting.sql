-- Preserve upstream price observations in their native channel-balance unit
-- while snapshotting the conversion inputs and accounting-currency values.

ALTER TABLE provider_price_rows
    ADD COLUMN source_unit TEXT NOT NULL DEFAULT 'CHANNEL_BALANCE_UNIT';
ALTER TABLE provider_price_rows
    ADD COLUMN billing_currency TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN recharge_multiplier TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_currency TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_input_per_million TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_output_per_million TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_cache_read_per_million TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_cache_write_per_million TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_flat_per_request TEXT;
ALTER TABLE provider_price_rows
    ADD COLUMN accounting_settings_version BIGINT;
ALTER TABLE provider_price_rows
    ADD COLUMN conversion_error TEXT;

ALTER TABLE provider_price_rows
    ADD CONSTRAINT provider_price_rows_source_unit_known
    CHECK (source_unit = 'CHANNEL_BALANCE_UNIT');
ALTER TABLE provider_price_rows
    ADD CONSTRAINT provider_price_rows_billing_currency_iso
    CHECK (billing_currency IS NULL OR billing_currency ~ '^[A-Z]{3}$');
ALTER TABLE provider_price_rows
    ADD CONSTRAINT provider_price_rows_accounting_currency_iso
    CHECK (accounting_currency IS NULL OR accounting_currency ~ '^[A-Z]{3}$');

COMMENT ON COLUMN provider_price_rows.currency IS
    'Deprecated legacy field. Native upstream values are channel balance units, not currency.';
COMMENT ON COLUMN provider_price_rows.source_unit IS
    'Unit of the unprefixed observed price columns; currently CHANNEL_BALANCE_UNIT.';
COMMENT ON COLUMN provider_price_rows.recharge_multiplier IS
    'Snapshot of channel balance units received per one billing-currency unit.';
COMMENT ON COLUMN provider_price_rows.accounting_settings_version IS
    'Accounting settings/rate version used for the snapshotted conversion.';

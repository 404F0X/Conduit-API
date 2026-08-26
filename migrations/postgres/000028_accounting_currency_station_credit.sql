-- Final fresh-schema money model:
--   * imported channel prices require an explicit real-world currency code;
--   * every customer/project ledger amount uses one stable internal unit code.
--
-- This migration intentionally contains no legacy data conversion. Development
-- databases using an older 000028 contract must be dropped and recreated.

ALTER TABLE channel_model_prices
    ADD COLUMN currency_code TEXT NOT NULL;
ALTER TABLE channel_model_price_versions
    ADD COLUMN currency_code TEXT NOT NULL;

ALTER TABLE channel_model_prices
    ADD CONSTRAINT channel_model_prices_currency_code_iso
    CHECK (currency_code ~ '^[A-Z]{3}$');

ALTER TABLE channel_model_price_versions
    ADD CONSTRAINT channel_model_price_versions_currency_code_iso
    CHECK (currency_code ~ '^[A-Z]{3}$');

ALTER TABLE project_wallets
    ADD CONSTRAINT project_wallets_station_credit_currency
    CHECK (currency = 'STATION_CREDIT');

ALTER TABLE credit_accounts
    ADD CONSTRAINT credit_accounts_station_credit_currency
    CHECK (currency = 'STATION_CREDIT');

ALTER TABLE subscription_plans
    ADD CONSTRAINT subscription_plans_station_credit_currency
    CHECK (currency = 'STATION_CREDIT');

ALTER TABLE customer_charge_events
    ADD CONSTRAINT customer_charge_events_station_credit_currency
    CHECK (currency = 'STATION_CREDIT');

ALTER TABLE project_commercial_profiles
    ADD CONSTRAINT project_commercial_profiles_station_credit_currency
    CHECK (billing_currency = 'STATION_CREDIT');

COMMENT ON COLUMN channel_model_prices.currency_code IS
    'Real-world accounting currency for the current import-price head.';
COMMENT ON COLUMN channel_model_price_versions.currency_code IS
    'Immutable real-world accounting currency for this import-price version.';
COMMENT ON COLUMN project_commercial_profiles.billing_currency IS
    'Stable station-credit ledger key; this is not a real-world accounting currency or a display name.';

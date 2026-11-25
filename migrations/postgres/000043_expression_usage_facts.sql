-- Authoritative outbound token facts retained across recovery and settlement.
ALTER TABLE usage_charge_outbox ADD COLUMN billing_token_facts JSONB;

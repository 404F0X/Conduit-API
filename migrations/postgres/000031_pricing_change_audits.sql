-- Immutable audit stream for procurement and retail pricing changes.

CREATE TABLE IF NOT EXISTS pricing_change_audits (
    id BIGSERIAL PRIMARY KEY,
    actor_type TEXT NOT NULL,
    actor_id BIGINT,
    operation TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    before_snapshot JSONB,
    after_snapshot JSONB,
    source_snapshot_id BIGINT,
    source_observation_id BIGINT,
    source_change_set_id BIGINT,
    accounting_currency TEXT NOT NULL,
    accounting_settings_version BIGINT NOT NULL,
    result TEXT NOT NULL,
    error_message TEXT,
    request_correlation_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS pricing_change_audits_entity_time
    ON pricing_change_audits (entity_type, entity_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS pricing_change_audits_actor_time
    ON pricing_change_audits (actor_type, actor_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS pricing_change_audits_correlation
    ON pricing_change_audits (request_correlation_id, id);

ALTER TABLE pricing_change_audits
    ADD CONSTRAINT pricing_change_audits_accounting_currency_iso
    CHECK (accounting_currency ~ '^[A-Z]{3}$');
ALTER TABLE pricing_change_audits
    ADD CONSTRAINT pricing_change_audits_settings_version_positive
    CHECK (accounting_settings_version > 0);

CREATE OR REPLACE FUNCTION reject_pricing_change_audit_mutation()
RETURNS trigger AS $body$
BEGIN
    RAISE EXCEPTION 'pricing_change_audits is append-only';
END;
$body$ LANGUAGE plpgsql;

CREATE TRIGGER pricing_change_audits_append_only
    BEFORE UPDATE OR DELETE ON pricing_change_audits
    FOR EACH ROW EXECUTE FUNCTION reject_pricing_change_audit_mutation();

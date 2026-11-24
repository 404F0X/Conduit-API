-- Public keys identify independent models. Display names are intentionally nonunique.
DROP INDEX models_by_name;
CREATE INDEX models_by_name ON models(name, deleted_at);

CREATE TABLE retail_price_formulas (
    id BIGSERIAL PRIMARY KEY,
    price_book_id BIGINT NOT NULL REFERENCES price_books(id),
    public_model_id BIGINT NOT NULL REFERENCES models(id),
    deployment_id BIGINT NOT NULL REFERENCES upstream_model_deployments(id),
    expression TEXT NOT NULL,
    parameters JSONB NOT NULL DEFAULT '{}'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT false,
    revision BIGINT NOT NULL DEFAULT 1,
    created_by BIGINT NOT NULL REFERENCES users(id),
    status TEXT NOT NULL DEFAULT 'DISABLED',
    last_error TEXT,
    last_fingerprint TEXT,
    last_change_set_id BIGINT REFERENCES change_sets(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(price_book_id, public_model_id),
    CHECK (length(expression) BETWEEN 1 AND 1024),
    CHECK (jsonb_typeof(parameters) = 'object')
);
ALTER TABLE change_sets ADD COLUMN formula_id BIGINT REFERENCES retail_price_formulas(id);
CREATE INDEX change_sets_formula ON change_sets(formula_id) WHERE formula_id IS NOT NULL;
ALTER TABLE project_wallet_reservations ADD COLUMN pricing_snapshot JSONB;

-- Use the existing accounting/price-write lock, before row locks, so a frozen
-- formula input cannot change between validation and publication.
CREATE FUNCTION lock_formula_inputs() RETURNS trigger AS $body$
BEGIN
    PERFORM pg_advisory_xact_lock(786222001);
    RETURN NULL;
END;
$body$ LANGUAGE plpgsql;

CREATE FUNCTION invalidate_formula_inputs() RETURNS trigger AS $body$
DECLARE
    old_row JSONB := CASE WHEN TG_OP = 'INSERT' THEN '{}'::jsonb ELSE to_jsonb(OLD) END;
    new_row JSONB := CASE WHEN TG_OP = 'DELETE' THEN '{}'::jsonb ELSE to_jsonb(NEW) END;
    affected BIGINT[];
BEGIN
    IF old_row - 'updated_at' = new_row - 'updated_at' THEN RETURN NULL; END IF;
    IF TG_TABLE_NAME = 'systems' AND COALESCE(new_row->>'key',old_row->>'key') <> 'system_general_settings' THEN RETURN NULL; END IF;
    SELECT array_agg(f.id) INTO affected FROM retail_price_formulas f
    WHERE f.enabled AND (
        (TG_TABLE_NAME = 'models' AND f.public_model_id = COALESCE((new_row->>'id')::bigint,(old_row->>'id')::bigint)) OR
        (TG_TABLE_NAME = 'model_routes' AND f.public_model_id IN ((new_row->>'public_model_id')::bigint,(old_row->>'public_model_id')::bigint)) OR
        (TG_TABLE_NAME = 'upstream_model_deployments' AND f.deployment_id = COALESCE((new_row->>'id')::bigint,(old_row->>'id')::bigint)) OR
        (TG_TABLE_NAME = 'channels' AND EXISTS(SELECT 1 FROM upstream_model_deployments d WHERE d.id=f.deployment_id AND d.channel_id=COALESCE((new_row->>'id')::bigint,(old_row->>'id')::bigint))) OR
        (TG_TABLE_NAME = 'price_books' AND f.price_book_id=COALESCE((new_row->>'id')::bigint,(old_row->>'id')::bigint)) OR
        TG_TABLE_NAME = 'systems'
    );
    UPDATE retail_price_formulas SET status='SOURCE_CHANGED',last_error='Source configuration changed; save the formula again to recalculate',updated_at=now() WHERE id=ANY(affected);
    WITH retired AS (
        UPDATE change_sets SET status='superseded',reviewed_at=now(),validation_error=NULL,updated_at=now()
        WHERE formula_id=ANY(affected) AND status IN ('draft','pending_review','approved') RETURNING id
    ) INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at)
      SELECT id,'formula_source_changed','system',jsonb_build_object('table',TG_TABLE_NAME),now() FROM retired;
    RETURN NULL;
END;
$body$ LANGUAGE plpgsql;

DO $body$
DECLARE relation_name TEXT;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY['models','model_routes','upstream_model_deployments','channels','price_books','systems'] LOOP
        EXECUTE format('CREATE TRIGGER formula_input_lock BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION lock_formula_inputs()',relation_name);
        EXECUTE format('CREATE TRIGGER formula_input_invalidation AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION invalidate_formula_inputs()',relation_name);
    END LOOP;
END;
$body$;

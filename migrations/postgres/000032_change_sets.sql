-- Unified review workflow. Pending changes never live in formal domain tables;
-- each kind is validated and applied transactionally by its domain applier.

CREATE TABLE IF NOT EXISTS change_sets (
    id BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    scope_type TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft',
    base_revision TEXT NOT NULL DEFAULT '',
    source_revision TEXT NOT NULL DEFAULT '',
    applied_target_type TEXT,
    applied_target_id TEXT,
    validation_error TEXT,
    created_by BIGINT,
    submitted_by BIGINT,
    reviewed_by BIGINT,
    review_note TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    submitted_at TIMESTAMPTZ,
    reviewed_at TIMESTAMPTZ,
    applied_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS change_set_items (
    id BIGSERIAL PRIMARY KEY,
    change_set_id BIGINT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
    item_key TEXT NOT NULL,
    action TEXT NOT NULL,
    before_snapshot JSONB,
    after_snapshot JSONB,
    source_snapshot JSONB,
    validation_error TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (change_set_id, item_key)
);

CREATE TABLE IF NOT EXISTS change_set_events (
    id BIGSERIAL PRIMARY KEY,
    change_set_id BIGINT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    actor_type TEXT NOT NULL,
    actor_id BIGINT,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS change_sets_review_queue
    ON change_sets (status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS change_sets_scope
    ON change_sets (kind, scope_type, scope_id, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS change_sets_activity
    ON change_sets (updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS change_sets_applied_target
    ON change_sets (kind, applied_target_type, applied_target_id, applied_at DESC, id DESC)
    WHERE status = 'applied' AND applied_target_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS change_set_items_parent
    ON change_set_items (change_set_id, id);
CREATE INDEX IF NOT EXISTS change_set_items_lookup
    ON change_set_items (item_key, change_set_id);
CREATE INDEX IF NOT EXISTS change_set_events_parent
    ON change_set_events (change_set_id, id);

ALTER TABLE change_sets
    ADD CONSTRAINT change_sets_kind_known
    CHECK (kind IN ('provider_price', 'model_mapping', 'retail_price'));
ALTER TABLE change_sets
    ADD CONSTRAINT change_sets_status_known
    CHECK (status IN ('draft', 'pending_review', 'applied', 'rejected', 'superseded', 'invalid'));
ALTER TABLE change_sets
    ADD CONSTRAINT change_sets_applied_target_complete
    CHECK ((applied_target_type IS NULL) = (applied_target_id IS NULL));
ALTER TABLE change_sets
    ADD CONSTRAINT change_sets_validation_state
    CHECK ((status = 'invalid') = (validation_error IS NOT NULL));
ALTER TABLE change_sets
    ADD CONSTRAINT change_sets_review_state
    CHECK (
        (status IN ('applied', 'rejected', 'superseded') AND reviewed_at IS NOT NULL)
        OR
        (status NOT IN ('applied', 'rejected', 'superseded'))
    );
ALTER TABLE change_set_items
    ADD CONSTRAINT change_set_items_action_known
    CHECK (action IN ('create', 'update', 'delete'));
ALTER TABLE change_set_events
    ADD CONSTRAINT change_set_events_actor_known
    CHECK (actor_type IN ('system', 'user'));

CREATE OR REPLACE FUNCTION reject_change_set_event_mutation()
RETURNS trigger AS $body$
BEGIN
    RAISE EXCEPTION 'change_set_events is append-only';
END;
$body$ LANGUAGE plpgsql;

CREATE TRIGGER change_set_events_append_only
    BEFORE UPDATE OR DELETE ON change_set_events
    FOR EACH ROW EXECUTE FUNCTION reject_change_set_event_mutation();

COMMENT ON TABLE change_sets IS
    'Unified draft and review workflow for provider prices, model mappings, and retail prices.';
COMMENT ON TABLE change_set_items IS
    'Typed-by-kind before/after/source payloads; domain appliers validate every snapshot before applying.';

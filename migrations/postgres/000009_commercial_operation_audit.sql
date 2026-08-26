-- Append-only audit trail for high-risk credit and subscription mutations.
-- Target identifiers intentionally remain TEXT so failed requests with an
-- invalid GraphQL ID are still auditable instead of being discarded.

CREATE TABLE IF NOT EXISTS commercial_operation_audits (
    id BIGSERIAL PRIMARY KEY,
    actor_type TEXT NOT NULL,
    actor_id TEXT,
    operation TEXT NOT NULL,
    target_project_id TEXT,
    target_user_id TEXT,
    amount TEXT,
    currency TEXT,
    plan_id TEXT,
    plan_name TEXT,
    subscription_id TEXT,
    idempotency_key TEXT,
    result TEXT NOT NULL,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS commercial_operation_audits_actor
    ON commercial_operation_audits (actor_type, actor_id, id);
CREATE INDEX IF NOT EXISTS commercial_operation_audits_project
    ON commercial_operation_audits (target_project_id, id);
CREATE INDEX IF NOT EXISTS commercial_operation_audits_operation
    ON commercial_operation_audits (operation, id);
CREATE INDEX IF NOT EXISTS commercial_operation_audits_idempotency
    ON commercial_operation_audits (idempotency_key, id);

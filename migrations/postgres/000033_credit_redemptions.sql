-- One-time credit redemption codes. Plaintext codes are returned once by the
-- application and are never persisted; only a SHA-256 lookup digest and a
-- non-secret display hint reach PostgreSQL.

CREATE TABLE IF NOT EXISTS credit_redemption_batches (
    id BIGSERIAL PRIMARY KEY,
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    currency TEXT NOT NULL DEFAULT 'STATION_CREDIT'
        CHECK (currency = 'STATION_CREDIT'),
    quantity INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 1000),
    expires_at TIMESTAMPTZ,
    description TEXT,
    created_by_actor_type TEXT NOT NULL,
    created_by_actor_id TEXT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS credit_redemption_batches_created
    ON credit_redemption_batches (created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS credit_redemption_codes (
    id BIGSERIAL PRIMARY KEY,
    batch_id BIGINT NOT NULL
        REFERENCES credit_redemption_batches(id) ON DELETE RESTRICT,
    code_digest TEXT NOT NULL UNIQUE
        CHECK (code_digest ~ '^sha256:[0-9a-f]{64}$'),
    code_hint TEXT NOT NULL CHECK (code_hint <> ''),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'redeemed', 'revoked')),
    redeemed_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT credit_redemption_codes_status_timestamps CHECK (
        (status = 'active' AND redeemed_at IS NULL AND revoked_at IS NULL)
        OR (status = 'redeemed' AND redeemed_at IS NOT NULL AND revoked_at IS NULL)
        OR (status = 'revoked' AND revoked_at IS NOT NULL AND redeemed_at IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS credit_redemption_codes_batch
    ON credit_redemption_codes (batch_id, id);
CREATE INDEX IF NOT EXISTS credit_redemption_codes_listing
    ON credit_redemption_codes (created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS credit_redemption_receipts (
    id BIGSERIAL PRIMARY KEY,
    code_id BIGINT NOT NULL UNIQUE
        REFERENCES credit_redemption_codes(id) ON DELETE RESTRICT,
    project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    wallet_id BIGINT NOT NULL REFERENCES project_wallets(id) ON DELETE RESTRICT,
    ledger_entry_id BIGINT NOT NULL UNIQUE
        REFERENCES project_credit_ledger_entries(id) ON DELETE RESTRICT,
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    currency TEXT NOT NULL CHECK (currency = 'STATION_CREDIT'),
    redeemed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS credit_redemption_receipts_project
    ON credit_redemption_receipts (project_id, redeemed_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS credit_redemption_receipts_user
    ON credit_redemption_receipts (user_id, redeemed_at DESC, id DESC);

-- Transaction-local audit rows accompany successful create, revoke, redeem,
-- and same-owner replay outcomes. detail_snapshot must contain identifiers and
-- business metadata only; raw redemption codes are forbidden.
CREATE TABLE IF NOT EXISTS credit_redemption_transaction_audits (
    id BIGSERIAL PRIMARY KEY,
    operation TEXT NOT NULL
        CHECK (operation IN ('create_codes', 'revoke_code', 'redeem_code')),
    actor_type TEXT NOT NULL,
    actor_id TEXT,
    batch_id BIGINT REFERENCES credit_redemption_batches(id) ON DELETE RESTRICT,
    code_id BIGINT REFERENCES credit_redemption_codes(id) ON DELETE RESTRICT,
    receipt_id BIGINT REFERENCES credit_redemption_receipts(id) ON DELETE RESTRICT,
    project_id BIGINT REFERENCES projects(id) ON DELETE RESTRICT,
    user_id BIGINT REFERENCES users(id) ON DELETE RESTRICT,
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'replayed')),
    detail_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(detail_snapshot) = 'object'),
    created_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT credit_redemption_transaction_audits_shape CHECK (
        (operation = 'create_codes' AND batch_id IS NOT NULL
            AND code_id IS NULL AND receipt_id IS NULL
            AND project_id IS NULL AND user_id IS NULL)
        OR (operation = 'revoke_code' AND batch_id IS NOT NULL
            AND code_id IS NOT NULL AND receipt_id IS NULL
            AND project_id IS NULL AND user_id IS NULL)
        OR (operation = 'redeem_code' AND batch_id IS NOT NULL
            AND code_id IS NOT NULL AND receipt_id IS NOT NULL
            AND project_id IS NOT NULL AND user_id IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS credit_redemption_transaction_audits_code
    ON credit_redemption_transaction_audits (code_id, id);
CREATE INDEX IF NOT EXISTS credit_redemption_transaction_audits_actor
    ON credit_redemption_transaction_audits (actor_type, actor_id, id);
CREATE INDEX IF NOT EXISTS credit_redemption_transaction_audits_project
    ON credit_redemption_transaction_audits (project_id, id);

CREATE OR REPLACE FUNCTION conduit_reject_credit_redemption_history_mutation()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END;
$$;

CREATE OR REPLACE FUNCTION conduit_enforce_credit_redemption_code_transition()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'credit redemption codes cannot be deleted';
    END IF;
    IF NEW.batch_id IS DISTINCT FROM OLD.batch_id
       OR NEW.code_digest IS DISTINCT FROM OLD.code_digest
       OR NEW.code_hint IS DISTINCT FROM OLD.code_hint
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'credit redemption code identity is immutable';
    END IF;
    IF OLD.status <> 'active'
       OR NEW.status NOT IN ('redeemed', 'revoked') THEN
        RAISE EXCEPTION 'invalid credit redemption code status transition: % -> %',
            OLD.status, NEW.status;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER credit_redemption_batches_append_only
BEFORE UPDATE OR DELETE ON credit_redemption_batches
FOR EACH ROW EXECUTE FUNCTION conduit_reject_credit_redemption_history_mutation();

CREATE TRIGGER credit_redemption_codes_state_machine
BEFORE UPDATE OR DELETE ON credit_redemption_codes
FOR EACH ROW EXECUTE FUNCTION conduit_enforce_credit_redemption_code_transition();

CREATE TRIGGER credit_redemption_receipts_append_only
BEFORE UPDATE OR DELETE ON credit_redemption_receipts
FOR EACH ROW EXECUTE FUNCTION conduit_reject_credit_redemption_history_mutation();

CREATE TRIGGER credit_redemption_transaction_audits_append_only
BEFORE UPDATE OR DELETE ON credit_redemption_transaction_audits
FOR EACH ROW EXECUTE FUNCTION conduit_reject_credit_redemption_history_mutation();

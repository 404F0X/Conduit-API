-- Payment receipts fund the existing Project wallet through its ledger trigger.
ALTER TABLE user_groups ADD COLUMN recharge_multiplier_ppm BIGINT NOT NULL DEFAULT 1000000
 CHECK (recharge_multiplier_ppm BETWEEN 1 AND 1000000000);
ALTER TABLE user_groups ADD COLUMN recharge_revision BIGINT NOT NULL DEFAULT 1 CHECK (recharge_revision > 0);
ALTER TABLE project_credit_ledger_entries ADD CONSTRAINT payment_topup_ledger_shape CHECK (
 entry_type<>'payment_topup' OR
 (amount_micros>0 AND reference_type IS NOT NULL AND reference_type='payment_order' AND reference_id IS NOT NULL
  AND idempotency_key='payment-topup:' || reference_id)
);
CREATE UNIQUE INDEX payment_topup_one_ledger_per_order ON project_credit_ledger_entries(reference_id)
 WHERE entry_type='payment_topup';

CREATE TABLE payment_orders (
 id UUID PRIMARY KEY,
 payer_user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
 project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
 idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
 provider TEXT NOT NULL CHECK (provider='epay-classic-md5'),
 merchant_identity TEXT NOT NULL CHECK (length(merchant_identity) BETWEEN 1 AND 128),
 provider_config TEXT NOT NULL CHECK (jsonb_typeof(provider_config::jsonb)='object'),
 channel TEXT NOT NULL CHECK (channel IN ('alipay','wechat')),
 currency TEXT NOT NULL CHECK (currency='CNY'),
 amount_minor BIGINT NOT NULL CHECK (amount_minor>0),
 credits_micros BIGINT NOT NULL CHECK (credits_micros>0),
 frozen_accounting TEXT NOT NULL CHECK (jsonb_typeof(frozen_accounting::jsonb)='object'),
 status TEXT NOT NULL CHECK (status IN ('pending','creation_unknown','closed','review_required','credited')),
 provider_transaction_id TEXT CHECK (length(provider_transaction_id) BETWEEN 1 AND 128),
 ledger_entry_id BIGINT UNIQUE REFERENCES project_credit_ledger_entries(id) ON DELETE RESTRICT,
 review_reason TEXT CHECK (review_reason IN ('payment_mismatch','target_closed','late_paid','manual_review_required','transaction_reused')),
 expires_at TIMESTAMPTZ NOT NULL,
 paid_at TIMESTAMPTZ,
 created_at TIMESTAMPTZ NOT NULL,
 updated_at TIMESTAMPTZ NOT NULL,
 UNIQUE(payer_user_id,idempotency_key),
 UNIQUE(provider,merchant_identity,provider_transaction_id),
 CHECK ((status='credited')=(ledger_entry_id IS NOT NULL)),
 CHECK (status<>'credited' OR (paid_at IS NOT NULL AND provider_transaction_id IS NOT NULL AND review_reason IS NULL)),
 CHECK (status<>'review_required' OR review_reason IS NOT NULL),
 CHECK (expires_at>created_at)
);
CREATE INDEX payment_orders_payer_listing ON payment_orders(payer_user_id,project_id,created_at DESC,id);

CREATE TABLE payment_order_events (
 id BIGSERIAL PRIMARY KEY,
 order_id UUID NOT NULL REFERENCES payment_orders(id) ON DELETE RESTRICT,
 outcome TEXT NOT NULL CHECK (outcome IN ('pending','creation_unknown','closed','review_required','credited')),
 reason TEXT,
 verified_evidence JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(verified_evidence)='object'),
 created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX payment_order_events_order ON payment_order_events(order_id,id);

CREATE FUNCTION conduit_payment_event_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'payment order events are append-only';
END;
$$;
CREATE TRIGGER payment_events_append_only BEFORE UPDATE OR DELETE ON payment_order_events
 FOR EACH ROW EXECUTE FUNCTION conduit_payment_event_immutable();

CREATE FUNCTION conduit_payment_order_frozen() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.id,NEW.payer_user_id,NEW.project_id,NEW.idempotency_key,NEW.provider,NEW.merchant_identity,NEW.provider_config,
        NEW.channel,NEW.currency,NEW.amount_minor,NEW.credits_micros,NEW.frozen_accounting,NEW.expires_at,NEW.created_at)
 IS DISTINCT FROM ROW(OLD.id,OLD.payer_user_id,OLD.project_id,OLD.idempotency_key,OLD.provider,OLD.merchant_identity,OLD.provider_config,
        OLD.channel,OLD.currency,OLD.amount_minor,OLD.credits_micros,OLD.frozen_accounting,OLD.expires_at,OLD.created_at) THEN
  RAISE EXCEPTION 'payment order accounting and identity are immutable';
 END IF;
 IF OLD.status='credited' AND NEW IS DISTINCT FROM OLD THEN
  RAISE EXCEPTION 'credited payment orders are immutable';
 END IF;
 IF OLD.provider_transaction_id IS NOT NULL AND NEW.provider_transaction_id IS DISTINCT FROM OLD.provider_transaction_id THEN
  RAISE EXCEPTION 'verified payment transaction identity is immutable';
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER payment_orders_frozen BEFORE UPDATE ON payment_orders
 FOR EACH ROW EXECUTE FUNCTION conduit_payment_order_frozen();

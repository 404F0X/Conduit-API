-- O(1) Project-credit balance and reservation snapshots for the PostgreSQL
-- request hot path. The append-only ledger and reservation lifecycle remain
-- authoritative; triggers make the snapshots impossible to bypass from a new
-- writer that inserts through the same database contracts.

ALTER TABLE project_wallets
    ADD COLUMN IF NOT EXISTS credit_balance_micros BIGINT NOT NULL DEFAULT 0;
ALTER TABLE project_wallets
    ADD COLUMN IF NOT EXISTS credit_reserved_micros BIGINT NOT NULL DEFAULT 0;

UPDATE project_wallets w
SET credit_balance_micros = COALESCE((
    SELECT SUM(e.amount_micros)::BIGINT
    FROM project_credit_ledger_entries e
    WHERE e.wallet_id = w.id
), 0);

UPDATE project_wallets w
SET credit_reserved_micros = COALESCE((
    SELECT SUM(a.reserved_micros)::BIGINT
    FROM project_wallet_reservation_allocations a
    JOIN project_wallet_reservations r ON r.id = a.reservation_id
    WHERE r.wallet_id = w.id
      AND a.source_type = 'project_credit'
      AND r.status IN ('reserved', 'shadow_reserved', 'capturing')
), 0);

ALTER TABLE project_wallets
    ADD CONSTRAINT project_wallets_credit_reserved_nonnegative
    CHECK (credit_reserved_micros >= 0);

CREATE OR REPLACE FUNCTION conduit_project_wallet_apply_ledger_entry()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    UPDATE project_wallets
    SET credit_balance_micros = credit_balance_micros + NEW.amount_micros,
        updated_at = GREATEST(updated_at, NEW.created_at)
    WHERE id = NEW.wallet_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'project wallet % not found for ledger entry', NEW.wallet_id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_credit_ledger_updates_wallet_balance
AFTER INSERT ON project_credit_ledger_entries
FOR EACH ROW EXECUTE FUNCTION conduit_project_wallet_apply_ledger_entry();

CREATE OR REPLACE FUNCTION conduit_project_wallet_reserve_allocation()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    reservation_wallet_id BIGINT;
    reservation_delta_micros BIGINT;
BEGIN
    IF NEW.source_type <> 'project_credit' THEN
        RETURN NEW;
    END IF;
    reservation_delta_micros := NEW.reserved_micros;
    IF TG_OP = 'UPDATE' THEN
        reservation_delta_micros := NEW.reserved_micros - OLD.reserved_micros;
    END IF;
    IF reservation_delta_micros = 0 THEN
        RETURN NEW;
    END IF;
    SELECT r.wallet_id INTO reservation_wallet_id
    FROM project_wallet_reservations r
    WHERE r.id = NEW.reservation_id
      AND r.status IN ('reserved', 'shadow_reserved', 'capturing');
    IF reservation_wallet_id IS NULL THEN
        RAISE EXCEPTION 'active reservation % not found for project-credit allocation', NEW.reservation_id;
    END IF;
    UPDATE project_wallets
    SET credit_reserved_micros = credit_reserved_micros + reservation_delta_micros
    WHERE id = reservation_wallet_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'project wallet % not found for reservation allocation', reservation_wallet_id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_credit_allocation_updates_wallet_reserved
AFTER INSERT ON project_wallet_reservation_allocations
FOR EACH ROW EXECUTE FUNCTION conduit_project_wallet_reserve_allocation();

CREATE TRIGGER project_credit_allocation_resize_updates_wallet_reserved
AFTER UPDATE OF reserved_micros ON project_wallet_reservation_allocations
FOR EACH ROW EXECUTE FUNCTION conduit_project_wallet_reserve_allocation();

CREATE OR REPLACE FUNCTION conduit_project_wallet_release_reservation()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    released_micros BIGINT;
BEGIN
    IF OLD.status NOT IN ('reserved', 'shadow_reserved', 'capturing')
       OR NEW.status IN ('reserved', 'shadow_reserved', 'capturing') THEN
        RETURN NEW;
    END IF;
    SELECT COALESCE(SUM(a.reserved_micros), 0)::BIGINT INTO released_micros
    FROM project_wallet_reservation_allocations a
    WHERE a.reservation_id = NEW.id
      AND a.source_type = 'project_credit';
    IF released_micros > 0 THEN
        UPDATE project_wallets
        SET credit_reserved_micros = credit_reserved_micros - released_micros
        WHERE id = NEW.wallet_id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_wallet_reservation_status_updates_reserved
AFTER UPDATE OF status ON project_wallet_reservations
FOR EACH ROW EXECUTE FUNCTION conduit_project_wallet_release_reservation();

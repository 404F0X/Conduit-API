-- Every accounting entrant takes governance first, matching policy-led writes.
-- Replace the function rather than existing migrations so installed schemas
-- retain their recorded checksums and existing trigger dependencies.
CREATE OR REPLACE FUNCTION lock_formula_inputs() RETURNS trigger AS $body$
BEGIN
    PERFORM pg_advisory_xact_lock(4850181378862437889);
    PERFORM pg_advisory_xact_lock(786222001);
    RETURN NULL;
END;
$body$ LANGUAGE plpgsql;

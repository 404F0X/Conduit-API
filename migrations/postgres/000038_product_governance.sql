-- Additive product controls. Existing packages and ledger entries keep their IDs.
ALTER TABLE change_sets DROP CONSTRAINT change_sets_kind_known;
ALTER TABLE change_sets ADD CONSTRAINT change_sets_kind_known CHECK (kind IN (
    'provider_price','model_mapping','retail_price','channel_configuration',
    'model_supply','group_access','identity_access','project_configuration',
    'system_configuration','project_credit'));
ALTER TABLE change_sets DROP CONSTRAINT change_sets_status_known;
ALTER TABLE change_sets ADD CONSTRAINT change_sets_status_known CHECK (status IN (
    'draft','pending_review','approved','applied','rejected','superseded','invalid'));
ALTER TABLE change_sets ADD COLUMN published_by BIGINT REFERENCES users(id);
ALTER TABLE change_sets ADD COLUMN published_at TIMESTAMPTZ;
-- An OFF publication has a publication receipt without fabricating a review.
-- Existing reviewed terminal rows retain their old meaning.
ALTER TABLE change_sets DROP CONSTRAINT change_sets_review_state;
ALTER TABLE change_sets ADD CONSTRAINT change_sets_review_state CHECK (
    (status='applied' AND (reviewed_at IS NOT NULL OR published_at IS NOT NULL))
    OR (status IN ('approved','rejected','superseded') AND reviewed_at IS NOT NULL)
    OR status IN ('draft','pending_review','invalid')
);


CREATE TABLE governed_change_commands (
    change_set_id BIGINT NOT NULL REFERENCES change_sets(id),
    item_key TEXT NOT NULL,
    action TEXT NOT NULL,
    private_payload JSONB NOT NULL CHECK (jsonb_typeof(private_payload)='object'),
    target_revision TEXT NOT NULL,
    domain_scope TEXT NOT NULL,
    project_ids BIGINT[] NOT NULL DEFAULT '{}',
    PRIMARY KEY(change_set_id,item_key)
);
COMMENT ON TABLE governed_change_commands IS
    'Private typed commands. Never expose payloads through items, events or error messages.';
CREATE TABLE governed_change_operations (
    operation_id UUID PRIMARY KEY,
    actor_id BIGINT NOT NULL REFERENCES users(id),
    action TEXT NOT NULL,
    fingerprint TEXT NOT NULL CHECK (fingerprint ~ '^[0-9a-f]{64}$'),
    change_set_id BIGINT NOT NULL REFERENCES change_sets(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Missing old mode means Enterprise; initialization explicitly replaces these
-- migration defaults with Lite and approval disabled in its own transaction.
INSERT INTO systems(key,value,created_at,updated_at) SELECT
    'system_product_experience_settings',
    '{"mode":"enterprise","revision":0,"overrides":{}}',now(),now()
WHERE EXISTS(SELECT 1 FROM systems WHERE key='system_initialized' AND value='true' AND deleted_at=0)
ON CONFLICT(key) DO NOTHING;
INSERT INTO systems(key,value,created_at,updated_at) SELECT
    'system_governance_settings',
    '{"revision":0,"enabled":true,"reviewKinds":["provider_price","model_mapping","retail_price"],"allowSelfReview":true,"legacyReviewScopes":true}',now(),now()
WHERE EXISTS(SELECT 1 FROM systems WHERE key='system_initialized' AND value='true' AND deleted_at=0)
ON CONFLICT(key) DO NOTHING;

CREATE TABLE project_member_spend_state (
    project_id BIGINT NOT NULL REFERENCES projects(id),
    user_id BIGINT NOT NULL REFERENCES users(id),
    limit_micros BIGINT CHECK (limit_micros >= 0),
    spent_micros BIGINT NOT NULL DEFAULT 0 CHECK (spent_micros >= 0),
    reserved_micros BIGINT NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(project_id,user_id)
);
-- State outlives membership removal; usage authority remains the existing
-- settlements/reservations. Budget units are Credits, never accounting cost.
INSERT INTO project_member_spend_state(project_id,user_id,spent_micros)
SELECT w.project_id,s.user_id,SUM(s.amount_micros)::BIGINT
FROM charge_settlements s JOIN project_wallets w ON w.id=s.wallet_id
WHERE s.user_id IS NOT NULL AND w.currency='STATION_CREDIT'
GROUP BY w.project_id,s.user_id;
INSERT INTO project_member_spend_state(project_id,user_id,reserved_micros)
SELECT w.project_id,r.user_id,SUM(r.amount_micros)::BIGINT
FROM project_wallet_reservations r JOIN project_wallets w ON w.id=r.wallet_id
WHERE r.status IN ('reserved','shadow_reserved','capturing','soft_insufficient')
  AND (r.expires_at>now() OR r.status='capturing') AND w.currency='STATION_CREDIT'
GROUP BY w.project_id,r.user_id
ON CONFLICT(project_id,user_id) DO UPDATE SET reserved_micros=EXCLUDED.reserved_micros;
CREATE INDEX project_member_reservations ON project_wallet_reservations(wallet_id,user_id,status,expires_at);

CREATE FUNCTION conduit_member_budget_admission() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    p BIGINT;
    budget project_member_spend_state%ROWTYPE;
    outstanding BIGINT;
BEGIN
    -- All billing paths use wallet -> member -> reservation -> bucket order.
    SELECT project_id INTO p FROM project_wallets
    WHERE id=NEW.wallet_id AND currency='STATION_CREDIT' FOR UPDATE;
    IF p IS NULL THEN RETURN NEW; END IF;
    INSERT INTO project_member_spend_state(project_id,user_id) VALUES(p,NEW.user_id)
    ON CONFLICT DO NOTHING;
    SELECT * INTO budget FROM project_member_spend_state
    WHERE project_id=p AND user_id=NEW.user_id FOR UPDATE;
    SELECT COALESCE(SUM(amount_micros),0)::BIGINT INTO outstanding
    FROM project_wallet_reservations
    WHERE wallet_id=NEW.wallet_id AND user_id=NEW.user_id
      AND status IN ('reserved','shadow_reserved','capturing','soft_insufficient')
      AND (expires_at>now() OR status='capturing');
    UPDATE project_member_spend_state SET reserved_micros=outstanding,updated_at=now()
    WHERE project_id=p AND user_id=NEW.user_id;
    IF NEW.amount_micros>0 AND budget.limit_micros IS NOT NULL
       AND budget.spent_micros+outstanding+NEW.amount_micros>budget.limit_micros THEN
        RAISE EXCEPTION USING ERRCODE='P0001',MESSAGE='MEMBER_SPEND_LIMIT_EXCEEDED';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_member_budget_admission BEFORE INSERT ON project_wallet_reservations
FOR EACH ROW EXECUTE FUNCTION conduit_member_budget_admission();

CREATE FUNCTION conduit_member_budget_reservation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p BIGINT;
BEGIN
    SELECT project_id INTO p FROM project_wallets WHERE id=NEW.wallet_id AND currency='STATION_CREDIT';
    IF p IS NULL THEN RETURN NEW; END IF;
    UPDATE project_member_spend_state SET reserved_micros=(
        SELECT COALESCE(SUM(amount_micros),0)::BIGINT FROM project_wallet_reservations
        WHERE wallet_id=NEW.wallet_id AND user_id=NEW.user_id
          AND status IN ('reserved','shadow_reserved','capturing','soft_insufficient')
          AND (expires_at>now() OR status='capturing')),updated_at=now()
    WHERE project_id=p AND user_id=NEW.user_id;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_member_budget_reservation AFTER INSERT OR UPDATE OF status,amount_micros,expires_at
ON project_wallet_reservations FOR EACH ROW EXECUTE FUNCTION conduit_member_budget_reservation();

CREATE FUNCTION conduit_member_budget_settlement() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p BIGINT;
BEGIN
    SELECT project_id INTO p FROM project_wallets WHERE id=NEW.wallet_id AND currency='STATION_CREDIT' FOR UPDATE;
    IF p IS NULL OR NEW.user_id IS NULL THEN RETURN NEW; END IF;
    -- Actual expense is retained even when it exceeds the admission estimate.
    INSERT INTO project_member_spend_state(project_id,user_id,spent_micros)
    VALUES(p,NEW.user_id,NEW.amount_micros)
    ON CONFLICT(project_id,user_id) DO UPDATE SET
        spent_micros=project_member_spend_state.spent_micros+EXCLUDED.spent_micros,updated_at=now();
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_member_budget_settlement AFTER INSERT ON charge_settlements
FOR EACH ROW EXECUTE FUNCTION conduit_member_budget_settlement();

CREATE TABLE channel_model_observations (
    id BIGSERIAL PRIMARY KEY,
    channel_id BIGINT NOT NULL REFERENCES channels(id),
    supported_models JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE project_commercial_profiles ADD COLUMN owner_user_id BIGINT REFERENCES users(id);
-- Backfill only unambiguous ownership. Ambiguous historical accounts require
-- an explicit administrative repair; neither names nor ID order choose money.
WITH owners AS (
 SELECT cp.project_id,MIN(up.user_id) AS user_id FROM project_commercial_profiles cp
 JOIN user_projects up ON up.project_id=cp.project_id AND up.is_owner
 WHERE cp.account_type='personal' AND cp.status='active' GROUP BY cp.project_id HAVING COUNT(*)=1
), unique_owners AS (
 SELECT user_id FROM owners GROUP BY user_id HAVING COUNT(*)=1
)
UPDATE project_commercial_profiles cp SET owner_user_id=o.user_id
FROM owners o JOIN unique_owners u ON u.user_id=o.user_id WHERE cp.project_id=o.project_id;
CREATE UNIQUE INDEX personal_project_owner ON project_commercial_profiles(owner_user_id)
WHERE account_type='personal' AND status='active' AND owner_user_id IS NOT NULL;
CREATE FUNCTION conduit_personal_project_owner() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE owner_id BIGINT; owner_count BIGINT;
BEGIN
 IF NEW.account_type='personal' AND NEW.owner_user_id IS NULL THEN
   SELECT MIN(user_id),COUNT(*) INTO owner_id,owner_count FROM user_projects WHERE project_id=NEW.project_id AND is_owner;
   IF owner_count=1 THEN NEW.owner_user_id:=owner_id; END IF;
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER personal_project_owner BEFORE INSERT ON project_commercial_profiles
FOR EACH ROW EXECUTE FUNCTION conduit_personal_project_owner();

-- Configuration guards are shared by GraphQL, HTTP, imports and workers.
-- A statement lock precedes target locks; row guards examine only the domain
-- fields, leaving accounting and health observations usable independently.
CREATE FUNCTION conduit_governance_statement_lock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(4850181378862437889);
    RETURN NULL;
END;
$$;
CREATE FUNCTION conduit_require_governance(k TEXT) RETURNS void LANGUAGE plpgsql AS $$
DECLARE p JSONB; package_id BIGINT;
BEGIN
    SELECT value::jsonb INTO p FROM systems WHERE key='system_governance_settings' AND deleted_at=0;
    IF p IS NULL THEN
        IF EXISTS(SELECT 1 FROM systems WHERE key='system_initialized' AND value='true' AND deleted_at=0) THEN
            p:='{"enabled":true,"reviewKinds":["provider_price","model_mapping","retail_price"]}'::jsonb;
        ELSE RETURN; END IF;
    END IF;
    IF NOT COALESCE((p->>'enabled')::boolean,false)
       OR NOT COALESCE(p->'reviewKinds' ? k,false) THEN RETURN; END IF;
    package_id := NULLIF(current_setting('conduit.governance_publish',true),'')::bigint;
    IF package_id IS NOT NULL AND EXISTS(SELECT 1 FROM change_sets
       WHERE id=package_id AND status IN ('approved','pending_review')) THEN RETURN; END IF;
    RAISE EXCEPTION USING ERRCODE='P0001',MESSAGE='APPROVAL_REQUIRED';
END;
$$;
CREATE FUNCTION conduit_governance_row_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE before_row JSONB; after_row JSONB; k TEXT := TG_ARGV[0]; setting_key TEXT;
        before_execution JSONB; after_execution JSONB;
BEGIN
    IF TG_OP='UPDATE' THEN
        before_row := to_jsonb(OLD)-ARRAY['updated_at','last_used_at','last_login_at'];
        after_row := to_jsonb(NEW)-ARRAY['updated_at','last_used_at','last_login_at'];
        IF before_row=after_row THEN RETURN NEW; END IF;
    END IF;
    IF TG_TABLE_NAME='systems' THEN
        setting_key := CASE WHEN TG_OP='DELETE' THEN OLD.key ELSE NEW.key END;
        IF setting_key IN ('system_initialized','system_onboarded') THEN RETURN COALESCE(NEW,OLD); END IF;
        IF setting_key='system_auto_backup_settings' AND TG_OP='UPDATE' AND (OLD.value::jsonb-ARRAY['last_backup_at','last_backup_error'])=(NEW.value::jsonb-ARRAY['last_backup_at','last_backup_error']) THEN RETURN NEW; END IF;
        IF setting_key='channel_model_mapping_automation_enabled' THEN PERFORM conduit_require_governance('model_mapping'); RETURN COALESCE(NEW,OLD); END IF;
        IF setting_key='system_governance_settings' AND current_setting('conduit.governance_policy_write',true)='authorized' THEN RETURN NEW; END IF;
        IF setting_key='system_product_experience_settings' THEN
            -- Mode/visibility are presentation. Execution overrides are governed.
            -- Compare named selections: identical value arrays can belong to
            -- different modules. Missing/INHERIT selections are equivalent.
            IF TG_OP IN ('INSERT','UPDATE') THEN
                IF TG_OP='UPDATE' THEN
                    before_row := OLD.value::jsonb;
                ELSE
                    -- INSERT triggers also run before ON CONFLICT UPDATE.
                    SELECT value::jsonb INTO before_row FROM systems
                      WHERE key=NEW.key AND deleted_at=0 FOR UPDATE;
                    before_row := COALESCE(before_row,'{}'::jsonb);
                END IF;
                SELECT COALESCE(jsonb_object_agg(key,value->'execution'),'{}'::jsonb) INTO before_execution
                  FROM jsonb_each(COALESCE(before_row->'overrides','{}'::jsonb))
                  WHERE COALESCE(value->>'execution','INHERIT')<>'INHERIT';
                SELECT COALESCE(jsonb_object_agg(key,value->'execution'),'{}'::jsonb) INTO after_execution
                  FROM jsonb_each(COALESCE(NEW.value::jsonb->'overrides','{}'::jsonb))
                  WHERE COALESCE(value->>'execution','INHERIT')<>'INHERIT';
                IF before_execution=after_execution THEN RETURN NEW; END IF;
            END IF;
        END IF;
    ELSIF TG_TABLE_NAME='channels' THEN
        IF TG_OP='INSERT' AND jsonb_array_length(COALESCE(NEW.settings->'modelMappings','[]'::jsonb))>0 THEN PERFORM conduit_require_governance('model_mapping'); END IF;
        IF TG_OP='INSERT' AND (jsonb_array_length(COALESCE(NEW.supported_models,'[]'::jsonb))>0 OR jsonb_array_length(COALESCE(NEW.manual_models,'[]'::jsonb))>0) THEN PERFORM conduit_require_governance('model_supply'); END IF;
        IF TG_OP='UPDATE' THEN
            IF COALESCE(OLD.settings->'modelMappings','[]'::jsonb) IS DISTINCT FROM COALESCE(NEW.settings->'modelMappings','[]'::jsonb) THEN PERFORM conduit_require_governance('model_mapping'); END IF;
            IF OLD.supported_models IS DISTINCT FROM NEW.supported_models OR OLD.manual_models IS DISTINCT FROM NEW.manual_models THEN PERFORM conduit_require_governance('model_supply'); END IF;
            IF (to_jsonb(OLD)-ARRAY['updated_at','supported_models','disabled_api_keys','quota','quota_checked_at','last_used_at'])
              =(to_jsonb(NEW)-ARRAY['updated_at','supported_models','disabled_api_keys','quota','quota_checked_at','last_used_at']) THEN
                IF OLD.supported_models IS DISTINCT FROM NEW.supported_models THEN PERFORM conduit_require_governance('model_supply'); END IF;
                IF OLD.disabled_api_keys IS DISTINCT FROM NEW.disabled_api_keys AND NOT(current_setting('conduit.health_restriction',true)='authorized' AND COALESCE(NULLIF(NEW.disabled_api_keys,'null'::jsonb),'[]'::jsonb) @> COALESCE(NULLIF(OLD.disabled_api_keys,'null'::jsonb),'[]'::jsonb)) THEN PERFORM conduit_require_governance('channel_configuration'); END IF;
                RETURN NEW;
            END IF;
        END IF;
    ELSIF TG_TABLE_NAME='users' THEN
        IF TG_OP='UPDATE' AND current_setting('conduit.self_user',true)=NEW.id::text AND (to_jsonb(OLD)-ARRAY['updated_at','last_login_at','first_name','last_name','prefer_language','avatar','password'])
          =(to_jsonb(NEW)-ARRAY['updated_at','last_login_at','first_name','last_name','prefer_language','avatar','password']) THEN RETURN NEW; END IF;
        IF TG_OP='INSERT' AND current_setting('conduit.auth_registration',true)='authorized'
           AND NOT NEW.is_owner AND NEW.scopes='[]'::jsonb THEN RETURN NEW; END IF;
    ELSIF TG_TABLE_NAME='api_keys' THEN
        IF current_setting('conduit.restricted_self_key',true)='authorized' THEN RETURN COALESCE(NEW,OLD); END IF;
    ELSIF TG_TABLE_NAME='project_credit_ledger_entries' THEN
        IF NEW.entry_type <> 'admin_grant' THEN RETURN NEW; END IF;
    ELSIF TG_TABLE_NAME='user_subscriptions' THEN
        -- Existing entitlement refresh/expiry does not create an entitlement.
        IF current_setting('conduit.issued_entitlement',true)='authorized' THEN RETURN COALESCE(NEW,OLD); END IF;
        IF TG_OP='UPDATE' AND NEW.status IN ('expired','cancelled')
           AND (to_jsonb(OLD)-ARRAY['status','updated_at'])=(to_jsonb(NEW)-ARRAY['status','updated_at']) THEN RETURN NEW; END IF;
    ELSIF TG_TABLE_NAME='credit_redemption_codes' THEN
        IF current_setting('conduit.issued_redemption',true)='authorized' AND TG_OP='UPDATE'
           AND NEW.status IN ('active','redeemed') AND OLD.status='active'
           AND (to_jsonb(OLD)-ARRAY['status','redeemed_at'])=(to_jsonb(NEW)-ARRAY['status','redeemed_at']) THEN RETURN NEW; END IF;
    ELSIF TG_TABLE_NAME='project_access_grants' THEN
        IF current_setting('conduit.issued_entitlement',true)='authorized' THEN RETURN COALESCE(NEW,OLD); END IF;
    ELSIF TG_TABLE_NAME='project_member_spend_state' THEN
        IF TG_OP='INSERT' AND NEW.limit_micros IS NULL THEN RETURN NEW; END IF;
        IF TG_OP='UPDATE' AND OLD.limit_micros IS NOT DISTINCT FROM NEW.limit_micros THEN RETURN NEW; END IF;
    END IF;
    -- Public signup owns only its newly-created personal workspace. Its fixed
    -- role cannot grant platform privileges or modify existing accounts.
    IF current_setting('conduit.auth_registration',true)='authorized' THEN
      IF TG_TABLE_NAME='projects' AND TG_OP='INSERT' THEN RETURN NEW; END IF;
      IF TG_TABLE_NAME IN ('user_projects','project_commercial_profiles','roles')
         AND TG_OP IN ('INSERT','UPDATE')
         AND to_jsonb(NEW)->>'project_id'=current_setting('conduit.registration_project',true) THEN
        IF TG_TABLE_NAME='user_projects' AND to_jsonb(NEW)->>'user_id'=current_setting('conduit.registration_user',true) THEN RETURN NEW; END IF;
        IF TG_TABLE_NAME='project_commercial_profiles' AND to_jsonb(NEW)->>'account_type'='personal'
           AND (to_jsonb(NEW)->>'owner_user_id' IS NULL OR to_jsonb(NEW)->>'owner_user_id'=current_setting('conduit.registration_user',true)) THEN RETURN NEW; END IF;
        IF TG_TABLE_NAME='roles' AND TG_OP='INSERT' AND to_jsonb(NEW)->>'level'='project'
           AND to_jsonb(NEW)->'scopes' <@ '["read_api_keys","write_api_keys","read_requests","write_requests"]'::jsonb THEN RETURN NEW; END IF;
      END IF;
      IF TG_TABLE_NAME='user_roles' AND TG_OP='INSERT'
         AND to_jsonb(NEW)->>'user_id'=current_setting('conduit.registration_user',true)
         AND EXISTS(SELECT 1 FROM roles WHERE id=(to_jsonb(NEW)->>'role_id')::bigint AND project_id::text=current_setting('conduit.registration_project',true)) THEN RETURN NEW; END IF;
    END IF;
    IF TG_TABLE_NAME='channels' AND current_setting('conduit.health_restriction',true)='authorized' AND TG_OP='UPDATE'
       AND (to_jsonb(OLD)-ARRAY['status','updated_at','disabled_api_keys','error_message'])=(to_jsonb(NEW)-ARRAY['status','updated_at','disabled_api_keys','error_message']) AND (to_jsonb(NEW)->>'status'=to_jsonb(OLD)->>'status' OR to_jsonb(NEW)->>'status'='disabled') AND COALESCE(NULLIF(to_jsonb(NEW)->'disabled_api_keys','null'::jsonb),'[]'::jsonb) @> COALESCE(NULLIF(to_jsonb(OLD)->'disabled_api_keys','null'::jsonb),'[]'::jsonb) THEN RETURN NEW; END IF;
    IF current_setting('conduit.emergency_restriction',true)='authorized' AND TG_OP='UPDATE'
       AND TG_TABLE_NAME IN ('channels','users','api_keys')
       AND (to_jsonb(OLD)-ARRAY['status','updated_at'])=(to_jsonb(NEW)-ARRAY['status','updated_at'])
       AND to_jsonb(NEW)->>'status' IN ('disabled','deactivated') THEN RETURN NEW; END IF;
    PERFORM conduit_require_governance(k);
    RETURN COALESCE(NEW,OLD);
END;
$$;
DO $$
DECLARE t TEXT; k TEXT;
BEGIN
    FOR t,k IN SELECT * FROM (VALUES
      ('channels','channel_configuration'),('models','model_supply'),
      ('upstream_model_deployments','model_supply'),('model_routes','model_supply'),
      ('simple_groups','group_access'),('simple_group_projects','group_access'),
      ('users','identity_access'),('roles','identity_access'),('user_roles','identity_access'),
      ('user_projects','identity_access'),('api_keys','identity_access'),
      ('projects','project_configuration'),('project_member_spend_state','project_configuration'),
      ('project_commercial_profiles','project_configuration'),('project_entitlement_overrides','project_configuration'),('project_price_adjustments','retail_price'),
      ('systems','system_configuration'),('subscription_plans','system_configuration'),
      ('user_subscriptions','system_configuration'),('credit_redemption_codes','system_configuration'),('credit_redemption_batches','system_configuration'),
      ('access_plans','project_configuration'),('access_plan_versions','project_configuration'),
      ('api_key_profile_templates','identity_access'),('channel_override_templates','channel_configuration'),
      ('prompt_protection_rules','system_configuration'),('data_storages','system_configuration'),
      ('access_plan_items','project_configuration'),('access_plan_route_items','project_configuration'),
      ('price_tiers','retail_price'),('project_access_grants','project_configuration'),
      ('subscription_plan_access_plans','system_configuration'),('subscription_quota_rules','system_configuration'),
      ('subscription_quota_rule_access_plans','system_configuration'),
      ('channel_model_prices','provider_price'),('price_books','retail_price'),
      ('price_book_versions','retail_price'),('price_book_items','retail_price')) AS v(t,k)
    LOOP
      IF to_regclass(t) IS NOT NULL THEN
        IF t<>'project_member_spend_state' THEN
          EXECUTE format('CREATE TRIGGER governance_lock BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION conduit_governance_statement_lock()',t);
        END IF;
        EXECUTE format('CREATE TRIGGER governance_guard BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION conduit_governance_row_guard(%L)',t,k);
      END IF;
    END LOOP;
END;
$$;
CREATE TRIGGER governance_credit_guard BEFORE INSERT ON project_credit_ledger_entries
FOR EACH ROW EXECUTE FUNCTION conduit_governance_row_guard('project_credit');

CREATE TABLE governed_change_secrets (
 change_set_id BIGINT PRIMARY KEY REFERENCES change_sets(id),
 recipient_user_id BIGINT NOT NULL REFERENCES users(id),
 private_payload JSONB NOT NULL,
 claimed_at TIMESTAMPTZ
);
COMMENT ON TABLE governed_change_secrets IS 'One-time authorized delivery; never included in public package receipts.';

-- Direct administrative identity writes cannot remove the last qualified
-- reviewer, even if IdentityAccess itself is excluded from reviewKinds.
CREATE FUNCTION conduit_protect_reviewers() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p JSONB; required_count INT; k TEXT; scope_name TEXT; actual_count INT;
BEGIN
 IF current_setting('conduit.emergency_restriction',true)='authorized' THEN RETURN NULL; END IF;
 SELECT value::jsonb INTO p FROM systems WHERE key='system_governance_settings' AND deleted_at=0;
 IF p IS NULL OR NOT COALESCE((p->>'enabled')::boolean,false) THEN RETURN NULL; END IF;
 required_count:=CASE WHEN COALESCE((p->>'allowSelfReview')::boolean,false) THEN 1 ELSE 2 END;
 FOR k,scope_name IN SELECT * FROM (VALUES
 ('provider_price','write_commercialization'),('retail_price','write_commercialization'),('model_mapping','write_commercialization'),
 ('channel_configuration','write_channels'),('model_supply','write_channels'),('group_access','write_groups'),
 ('identity_access','write_users'),('identity_access','write_roles'),('identity_access','write_api_keys'),
 ('project_configuration','write_projects'),('project_configuration','write_billing'),('project_configuration','write_commercialization'),
 ('system_configuration','write_settings'),('system_configuration','write_subscriptions'),('system_configuration','write_data_storages'),('system_configuration','grant_credit'),('project_credit','grant_credit')) v(k,s)
 LOOP
 IF p->'reviewKinds' ? k THEN
 SELECT COUNT(*) INTO actual_count FROM users u WHERE u.status='activated' AND u.deleted_at=0
 AND (u.is_owner OR u.scopes ? '*' OR u.scopes ? scope_name OR EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.deleted_at=0 AND (r.project_id IS NULL OR r.project_id=0) AND (r.scopes ? '*' OR r.scopes ? scope_name)))
 AND (COALESCE((p->>'legacyReviewScopes')::boolean,false) OR u.is_owner OR u.scopes ? '*' OR u.scopes ? 'review_changes' OR EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.deleted_at=0 AND (r.project_id IS NULL OR r.project_id=0) AND (r.scopes ? '*' OR r.scopes ? 'review_changes')));
 IF actual_count<required_count THEN RAISE EXCEPTION USING ERRCODE='P0001',MESSAGE='NO_ELIGIBLE_REVIEWER'; END IF;
 END IF;
 END LOOP;
 RETURN NULL;
END;
$$;
CREATE TRIGGER protect_reviewers AFTER UPDATE OR DELETE ON users FOR EACH STATEMENT EXECUTE FUNCTION conduit_protect_reviewers();
CREATE TRIGGER protect_reviewers AFTER UPDATE OR DELETE ON roles FOR EACH STATEMENT EXECUTE FUNCTION conduit_protect_reviewers();
CREATE TRIGGER protect_reviewers AFTER UPDATE OR DELETE ON user_roles FOR EACH STATEMENT EXECUTE FUNCTION conduit_protect_reviewers();

-- A new personal account obtains one explicit owner after its membership is
-- inserted. Shared accounts use account_type=shared and never auto-select.
CREATE FUNCTION conduit_link_personal_owner() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE existing_owner BIGINT; personal BOOLEAN;
BEGIN
 SELECT account_type='personal' AND status='active',owner_user_id INTO personal,existing_owner FROM project_commercial_profiles WHERE project_id=NEW.project_id FOR UPDATE;
 IF personal AND NEW.is_owner THEN
 IF existing_owner IS NOT NULL AND existing_owner<>NEW.user_id THEN RAISE EXCEPTION USING ERRCODE='P0001',MESSAGE='PERSONAL_PROJECT_OWNER_CONFLICT'; END IF;
 IF existing_owner IS NULL AND (SELECT COUNT(*) FROM user_projects WHERE project_id=NEW.project_id AND is_owner)=1 THEN
 UPDATE project_commercial_profiles SET owner_user_id=NEW.user_id,updated_at=now() WHERE project_id=NEW.project_id;
 END IF;
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER personal_owner_link AFTER INSERT OR UPDATE OF is_owner ON user_projects FOR EACH ROW EXECUTE FUNCTION conduit_link_personal_owner();

-- Read permissions follow the actual command domain and every affected Project.
-- Only server-parsed command metadata is stored here; private payloads stay private.
CREATE FUNCTION conduit_can_read_domain(actor BIGINT, scope TEXT, project BIGINT DEFAULT NULL)
RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM users u WHERE u.id=actor AND u.deleted_at=0 AND u.status='activated' AND (
   u.is_owner OR u.scopes ? '*' OR u.scopes ? scope OR u.scopes ? CASE WHEN scope='grant_credit' THEN 'read_billing' ELSE replace(scope,'write_','read_') END
   OR EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.deleted_at=0 AND (r.project_id IS NULL OR r.project_id=0) AND (r.scopes ? '*' OR r.scopes ? scope OR r.scopes ? CASE WHEN scope='grant_credit' THEN 'read_billing' ELSE replace(scope,'write_','read_') END))
   OR (project IS NOT NULL AND scope IN ('write_users','write_roles','write_api_keys','write_requests','write_prompts') AND EXISTS(SELECT 1 FROM user_projects up JOIN projects p ON p.id=up.project_id WHERE up.user_id=u.id AND p.id=project AND p.deleted_at=0 AND (up.is_owner OR up.scopes ? scope OR up.scopes ? replace(scope,'write_','read_') OR EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.project_id=project AND r.deleted_at=0 AND (r.scopes ? scope OR r.scopes ? replace(scope,'write_','read_'))))))
  ))
$$;
CREATE FUNCTION conduit_can_read_change_set(actor BIGINT, package_id BIGINT)
RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM change_sets cs WHERE cs.id=package_id AND
  CASE WHEN cs.scope_type='governed' THEN
    EXISTS(SELECT 1 FROM governed_change_commands c WHERE c.change_set_id=cs.id)
    AND NOT EXISTS(SELECT 1 FROM governed_change_commands c WHERE c.change_set_id=cs.id AND
       CASE WHEN cardinality(c.project_ids)=0 THEN NOT conduit_can_read_domain(actor,c.domain_scope)
       ELSE EXISTS(SELECT 1 FROM unnest(c.project_ids) p WHERE NOT conduit_can_read_domain(actor,c.domain_scope,p)) END)
  ELSE conduit_can_read_domain(actor,'write_commercialization') END)
$$;

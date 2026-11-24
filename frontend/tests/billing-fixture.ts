import { expect, type Page, type Locator } from '@playwright/test';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

export function sql(statement: string): string {
  const raw = process.env.CONDUIT_E2E_POSTGRES_DSN;
  if (process.env.CONDUIT_E2E_ISOLATED !== '1' || !raw) throw new Error('Run this spec through the isolated test:e2e harness');
  const dsn = new URL(raw);
  const database = decodeURIComponent(dsn.pathname.slice(1));
  if (
    !['postgres:', 'postgresql:'].includes(dsn.protocol) ||
    !['127.0.0.1', 'localhost', '[::1]'].includes(dsn.hostname) ||
    !/^conduit_e2e(?:_[a-z0-9][a-z0-9_]*)?$/.test(database) ||
    /(?:^|_)(?:prod|production|live|staging)(?:_|$)/.test(database)
  )
    throw new Error('Unsafe billing fixture database');
  const result = spawnSync('psql', ['--no-psqlrc', '--set', 'ON_ERROR_STOP=1', '--tuples-only', '--no-align', '--quiet'], {
    input: statement,
    encoding: 'utf8',
    timeout: 15000,
    windowsHide: true,
    env: {
      ...process.env,
      PGHOST: dsn.hostname.replaceAll(/[\[\]]/g, ''),
      PGPORT: dsn.port || '5432',
      PGUSER: decodeURIComponent(dsn.username),
      PGPASSWORD: decodeURIComponent(dsn.password),
      PGDATABASE: database,
    },
  });
  if (result.status !== 0) throw new Error(`Billing fixture SQL failed: ${result.error?.message ?? result.stderr}`);
  return result.stdout.trim();
}

export function seedBillingFixture(
  projectID: number,
  options: { suffix?: string; grantMicros?: string; rechargePPM?: string; conditional?: boolean } = {}
) {
  const suffix = options.suffix ?? 'billing';
  const grant = options.grantMicros ?? '1000000000';
  const ppm = options.rechargePPM ?? '1000000';
  if (
    !Number.isSafeInteger(projectID) ||
    projectID < 1 ||
    !/^[a-z]+$/.test(suffix) ||
    !/^(0|[1-9]\d*)$/.test(grant) ||
    !/^[1-9]\d*$/.test(ppm)
  )
    throw new Error('Invalid isolated fixture options');
  const groupID = randomUUID();
  const leaf = (prompt: string, completion: string) => ({
    items: [
      { itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: prompt } },
      { itemCode: 'completion_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: completion } },
      { itemCode: 'prompt_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '0' } },
      { itemCode: 'prompt_write_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '0' } },
    ],
  });
  const retailPrice = options.conditional
    ? {
        schema: 'group-conditional-price-v1',
        global: leaf('9', '10'),
        groups: [
          {
            modelGroupID: groupID,
            base: leaf('7', '8'),
            rules: [{ id: 'token-rule', label: 'Published token rule', condition: 'prompt_tokens >= 100', price: leaf('2', '4') }],
          },
        ],
      }
    : null;
  const upstream = process.env.CONDUIT_E2E_MOCK_UPSTREAM_URL;
  if (!upstream || !/^http:\/\/127\.0\.0\.1:\d+\/v1$/.test(upstream)) throw new Error('Missing isolated mock upstream');
  sql(`BEGIN;
    SELECT set_config('conduit.governance_policy_write','authorized',true);
    INSERT INTO systems(key,value) VALUES('system_general_settings','{"accounting_currency_code":"CNY","credit_display_name":"Credits","credits_per_accounting_unit":"10000","exchange_rates":[],"accounting_rate_version":1}')
      ON CONFLICT(key) DO UPDATE SET value=(systems.value::jsonb || EXCLUDED.value::jsonb)::text;
    DO $$
    #variable_conflict use_column
    DECLARE policy text; channel_id bigint; model_id bigint; route_id bigint; deployment_id bigint;
      plan_id bigint; access_version bigint; tier_id bigint; book_id bigint; version_id bigint; wallet_id bigint;
      price jsonb := '{"items":[
        {"itemCode":"prompt_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"2"}},
        {"itemCode":"completion_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"4"}},
        {"itemCode":"prompt_cached_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"0"}},
        {"itemCode":"prompt_write_cached_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"0"}}]}';
    BEGIN
      SELECT value INTO policy FROM systems WHERE key='system_governance_settings' AND deleted_at=0;
      INSERT INTO systems(key,value) VALUES('system_governance_settings','{"enabled":false}')
        ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value;
      INSERT INTO systems(key,value) VALUES('system_product_experience_settings','{"mode":"enterprise","revision":1}')
        ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value;
      INSERT INTO channels(type,name,status,base_url,credentials,supported_models,default_test_model,settings)
        VALUES('openai','E2E ${suffix} upstream','enabled','${upstream}','{"apiKey":"local-fixture-only"}',
          '["gpt-4o"]','gpt-4o','{"billingCurrency":"CNY","rechargeMultiplier":"1"}') RETURNING id INTO channel_id;
      INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,variant,status,source)
        VALUES(channel_id,'gpt-4o','E2E ${suffix} deployment','','enabled','test')
        ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET status='enabled' RETURNING id INTO deployment_id;
      INSERT INTO models(developer,model_id,type,name,icon,"group",model_card,settings,status)
        VALUES('test','conduit-e2e-${suffix}','chat','E2E ${suffix} model','','test','{}','{}','enabled') RETURNING id INTO model_id;
      INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES(model_id,deployment_id,'enabled') RETURNING id INTO route_id;
      INSERT INTO channel_model_prices(channel_id,model_id,price,reference_id,currency_code)
        VALUES(channel_id,'gpt-4o',price,'e2e-procurement-${suffix}','CNY');
      INSERT INTO access_plans(name,status,is_default,created_at,updated_at)
        VALUES('E2E ${suffix} access','enabled',false,now(),now()) RETURNING id INTO plan_id;
      INSERT INTO access_plan_versions(access_plan_id,version,status,reference_id,created_at,updated_at)
        VALUES(plan_id,1,'published','e2e-access-${suffix}',now(),now()) RETURNING id INTO access_version;
      INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES(access_version,model_id,now());
      INSERT INTO access_plan_route_items(access_plan_version_id,model_route_id,created_at) VALUES(access_version,route_id,now());
      INSERT INTO project_access_grants(project_id,access_plan_version_id,source_type,source_id,status,created_at,updated_at)
        VALUES(${projectID},access_version,'test','e2e-${suffix}','active',now(),now());
      INSERT INTO price_tiers(name,multiplier_ppm,status,is_default,created_at,updated_at)
        VALUES('E2E ${suffix} 1.5',1500000,'enabled',false,now(),now()) RETURNING id INTO tier_id;
      INSERT INTO project_commercial_profiles(project_id,account_type,base_price_tier_id,status,created_at,updated_at)
        VALUES(${projectID},'shared',tier_id,'active',now(),now())
        ON CONFLICT(project_id) DO UPDATE SET base_price_tier_id=EXCLUDED.base_price_tier_id;
      UPDATE price_books SET is_default=false WHERE is_default;
      INSERT INTO price_books(name,currency,status,is_default) VALUES('E2E ${suffix} published retail','CNY','enabled',true) RETURNING id INTO book_id;
      INSERT INTO price_book_versions(price_book_id,version,status,reference_id,effective_start_at)
        VALUES(book_id,1,'published','e2e-retail-published-${suffix}',now()-interval '1 minute') RETURNING id INTO version_id;
      INSERT INTO price_book_items(price_book_version_id,public_model_id,price) VALUES(version_id,model_id,${retailPrice ? `'${JSON.stringify(retailPrice)}'::jsonb` : 'price'});
      INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at)
        VALUES(${projectID},'STATION_CREDIT','active',now(),now())
        ON CONFLICT(project_id,currency) DO UPDATE SET status='active' RETURNING id INTO wallet_id;
      INSERT INTO project_credit_ledger_entries(wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at)
        VALUES(wallet_id,${grant},'admin_grant','e2e-${suffix}-grant','{}',now());
      INSERT INTO model_groups(id,name,status) VALUES('${groupID}','E2E ${suffix} group','enabled');
      INSERT INTO model_group_models(model_group_id,public_model_key) VALUES('${groupID}','conduit-e2e-${suffix}');
      INSERT INTO user_groups(id,name,status,recharge_multiplier_ppm) VALUES('${groupID}','E2E ${suffix} users','enabled',${ppm});
      INSERT INTO user_group_model_groups(user_group_id,model_group_id) VALUES('${groupID}','${groupID}');
      INSERT INTO user_group_users(user_id,user_group_id)
        SELECT id,'${groupID}' FROM users WHERE email='e2e-owner@conduit.invalid' AND deleted_at=0
        ON CONFLICT(user_id) DO UPDATE SET user_group_id=EXCLUDED.user_group_id;
      IF policy IS NULL THEN DELETE FROM systems WHERE key='system_governance_settings' AND deleted_at=0;
      ELSE UPDATE systems SET value=policy WHERE key='system_governance_settings' AND deleted_at=0; END IF;
    END $$;
    COMMIT;`);
  return { groupID, retailPrice, modelKey: `conduit-e2e-${suffix}`, modelName: `E2E ${suffix} model` };
}

// Each financial browser scenario gets its own wallet; the suite must not
// reset balances or erase other scenarios' ledger evidence.
export function createBillingProject(suffix: string): number {
  if (!/^[a-z]+$/.test(suffix)) throw new Error('Invalid fixture Project suffix');
  const value = sql(`WITH p AS (
    INSERT INTO projects(name,description,status) VALUES('E2E ${suffix} Project','','active') RETURNING id
  ), member AS (
    INSERT INTO user_projects(user_id,project_id,is_owner,scopes)
    SELECT u.id,p.id,true,'[]'::jsonb FROM users u CROSS JOIN p
    WHERE u.email='e2e-owner@conduit.invalid' AND u.deleted_at=0 RETURNING project_id
  ) SELECT project_id FROM member`);
  const id = Number(value);
  if (!Number.isSafeInteger(id) || id < 1) throw new Error('Fixture Project member was not created');
  return id;
}

export async function selectBillingModelGroup(page: Page, dialog: Locator, groupID: string) {
  if (!/^[a-f\d]{8}(?:-[a-f\d]{4}){3}-[a-f\d]{12}$/i.test(groupID)) throw new Error('Invalid fixture ModelGroup identity');
  const select = dialog.getByRole('combobox', { name: /所属模型组/ });
  await expect(select.locator(`option[value="${groupID}"]`)).toHaveCount(1, { timeout: 15000 });
  const responseReady = page.waitForResponse(
    (response) => {
      if (!response.url().endsWith('/admin/graphql')) return false;
      const body = response.request().postDataJSON();
      return body?.query?.includes('apiKeyCreationCatalog') && body.variables?.modelGroupID === groupID;
    },
    { timeout: 15000 }
  );
  await select.selectOption(groupID);
  const response = await responseReady;
  expect(response.status()).toBe(200);
  const result = await response.json();
  expect(result.errors ?? []).toEqual([]);
  expect(result.data?.apiKeyCreationCatalog?.configured).toBe(true);
  expect(result.data?.apiKeyCreationCatalog?.modelGroupID).toBe(groupID);
}

import { test, expect } from '@playwright/test';
import { gotoAndEnsureAuth } from './auth.utils';
import { sql, seedBillingFixture, createBillingProject, selectBillingModelGroup } from './billing-fixture';

// No traces/screenshots/video can persist the newly revealed fixture API key.
test.use({ trace: 'off', screenshot: 'off', video: 'off' });
test.describe.configure({ retries: 0 });

// 100 input + 20 output tokens at 2/4 CNY per 1M tokens, 10000 credits/CNY,
// Project multiplier 1.5 => 4.2 credits = exactly 4200000 microcredits.
const expectedDebit = 4_200_000;

test('normal user key: model market → Playground SSE → usage → one wallet debit', async ({ page }) => {
  test.setTimeout(120000);
  await gotoAndEnsureAuth(page, '/project/api-keys');
  await expect(page.locator('header')).toBeVisible();
  const projectID = createBillingProject('billing');
  await page.evaluate((id) => localStorage.setItem('conduit_selected_project_id', `gid://conduit/Project/${id}`), projectID);
  const { groupID } = seedBillingFixture(projectID);
  await page.reload();
  await expect(page.locator('header').getByRole('button', { name: 'E2E billing Project', exact: true })).toBeVisible();

  await page.getByRole('button', { name: /Create API Key|创建 API Key/i }).click();
  const createDialog = page.getByRole('dialog');
  await createDialog.getByLabel(/Name|名称/i, { exact: true }).fill('E2E billing browser key');
  await selectBillingModelGroup(page, createDialog, groupID);
  await createDialog.getByRole('button', { name: /Create|创建/i, exact: true }).click();
  const view = page.getByRole('dialog', { name: /View API Key|查看 API 密钥/i });
  await expect(view).toBeVisible();
  const code = view.locator('code').first();
  await code.locator('..').getByRole('button').first().click();
  const apiKey = (await code.textContent())?.trim();
  if (!apiKey || apiKey.includes('*')) throw new Error('Newly created key was not revealed');
  const keyID = Number(sql("SELECT id FROM api_keys WHERE name='E2E billing browser key' AND type='user' AND deleted_at=0"));
  expect(Number.isSafeInteger(keyID) && keyID > 0).toBe(true);

  await page.goto('/project/models');
  await page.getByLabel('Price display scope').selectOption(`group:${groupID}`);
  const card = page.getByRole('button', { name: 'View model E2E billing model' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('Credits 30000');
  await expect(card).toContainText('Credits 60000');
  await card.click();
  await expect(page.getByText(/current Project multiplier ×1.5/)).toBeVisible();

  await page.goto('/project/wallet');
  await expect(page.getByText('Credits 1000', { exact: true }).first()).toBeVisible();
  await page.goto('/project/playground');
  await page.getByLabel('API key', { exact: true }).fill(apiKey);
  const modelResponse = page.waitForResponse((response) => response.url().endsWith('/v1/models'));
  await page.getByRole('button', { name: 'Load available models', exact: true }).click();
  expect((await modelResponse).status()).toBe(200);
  await expect(page.getByRole('combobox')).toHaveValue('conduit-e2e-billing');
  await page.getByLabel('Max Tokens', { exact: true }).fill('32');
  await page.getByPlaceholder('Type a message...').fill('deterministic fixture question');
  const completion = page.waitForResponse((response) => response.url().endsWith('/v1/chat/completions'));
  await page.getByRole('button', { name: 'Submit', exact: true }).click();
  const stream = await completion;
  expect(stream.status()).toBe(200);
  expect(stream.headers()['content-type']).toContain('text/event-stream');
  await expect(page.getByText('local streaming billing response', { exact: true })).toBeVisible();

  const snapshot = () =>
    JSON.parse(
      sql(`SELECT json_build_object(
    'usageCount',(SELECT count(*) FROM usage_logs WHERE api_key_id=${keyID}),
    'prompt',(SELECT prompt_tokens FROM usage_logs WHERE api_key_id=${keyID} LIMIT 1),
    'completion',(SELECT completion_tokens FROM usage_logs WHERE api_key_id=${keyID} LIMIT 1),
    'chargeCount',(SELECT count(*) FROM customer_charge_events c JOIN usage_logs u ON u.id=c.usage_log_id WHERE u.api_key_id=${keyID}),
    'settledCount',(SELECT count(*) FROM charge_settlements s JOIN customer_charge_events c ON c.id=s.charge_event_id JOIN usage_logs u ON u.id=c.usage_log_id WHERE u.api_key_id=${keyID} AND s.status='settled'),
    'debitCount',(SELECT count(*) FROM project_credit_ledger_entries l JOIN usage_logs u ON l.reference_type='usage_log' AND l.reference_id=u.id::text WHERE u.api_key_id=${keyID} AND l.entry_type='usage_charge'),
    'debitMicros',(SELECT -sum(l.amount_micros) FROM project_credit_ledger_entries l JOIN usage_logs u ON l.reference_type='usage_log' AND l.reference_id=u.id::text WHERE u.api_key_id=${keyID} AND l.entry_type='usage_charge'),
    'reservedMicros',(SELECT credit_reserved_micros FROM project_wallets WHERE project_id=${projectID} AND currency='STATION_CREDIT'),
    'balanceMicros',(SELECT credit_balance_micros FROM project_wallets WHERE project_id=${projectID} AND currency='STATION_CREDIT'),
    'activeReservations',(SELECT count(*) FROM project_wallet_reservations r JOIN project_wallets w ON w.id=r.wallet_id WHERE w.project_id=${projectID} AND r.status IN ('reserved','shadow_reserved','capturing','soft_insufficient'))
  )`)
    );
  await expect
    .poll(snapshot, { timeout: 30000, intervals: [200, 500, 1000], message: 'one exact settled charge and no remaining reservation' })
    .toEqual({
      usageCount: 1,
      prompt: 100,
      completion: 20,
      chargeCount: 1,
      settledCount: 1,
      debitCount: 1,
      debitMicros: expectedDebit,
      reservedMicros: 0,
      balanceMicros: 1_000_000_000 - expectedDebit,
      activeReservations: 0,
    });
  await page.goto('/project/requests');
  await expect(page.getByTestId('requests-table')).toContainText('conduit-e2e-billing');
  await page.goto('/project/wallet');
  await expect(page.getByText('Credits 995.8', { exact: true }).first()).toBeVisible();
  // Database and browser assertions deliberately never print or attach the API key.
});

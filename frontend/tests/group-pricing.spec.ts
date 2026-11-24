import { test, expect } from '@playwright/test';
import { gotoAndEnsureAuth } from './auth.utils';
import { sql, seedBillingFixture, createBillingProject, selectBillingModelGroup } from './billing-fixture';

// No traces/screenshots/video can persist the newly revealed fixture API key.
test.use({ trace: 'off', screenshot: 'off', video: 'off' });
test.describe.configure({ retries: 0 });

// 100 input + 20 output tokens at 2/4 CNY per 1M tokens, 10000 credits/CNY,
// Project multiplier 1.5 => 4.2 credits = exactly 4200000 microcredits.
const expectedDebit = 4_200_000;

test('group conditional price: dialog draft survives refresh without publishing → scoped SSE charge', async ({ page }) => {
  test.setTimeout(120000);
  await gotoAndEnsureAuth(page, '/project/api-keys');
  await expect(page.locator('header')).toBeVisible();
  const projectID = createBillingProject('conditional');
  await page.evaluate((id) => localStorage.setItem('conduit_selected_project_id', `gid://conduit/Project/${id}`), projectID);
  const { groupID, retailPrice } = seedBillingFixture(projectID, { suffix: 'conditional', conditional: true });
  await page.reload();
  await expect(page.locator('header').getByRole('button', { name: 'E2E conditional Project', exact: true })).toBeVisible();

  await page.getByRole('button', { name: /Create API Key|创建 API Key/i }).click();
  const createDialog = page.getByRole('dialog');
  await createDialog.getByLabel(/Name|名称/i, { exact: true }).fill('E2E conditional browser key');
  await selectBillingModelGroup(page, createDialog, groupID);
  await createDialog.getByRole('button', { name: /Create|创建/i, exact: true }).click();
  const view = page.getByRole('dialog', { name: /View API Key|查看 API 密钥/i });
  await expect(view).toBeVisible();
  const code = view.locator('code').first();
  await code.locator('..').getByRole('button').first().click();
  const apiKey = (await code.textContent())?.trim();
  if (!apiKey || apiKey.includes('*')) throw new Error('Newly created key was not revealed');
  const keyID = Number(sql("SELECT id FROM api_keys WHERE name='E2E conditional browser key' AND type='user' AND deleted_at=0"));
  expect(Number.isSafeInteger(keyID) && keyID > 0).toBe(true);

  await page.goto('/project/models');
  await page.getByLabel('Price display scope').selectOption(`group:${groupID}`);
  const card = page.getByRole('button', { name: 'View model E2E conditional model' });
  await expect(card).toBeVisible();
  await card.click();
  await expect(page.getByText('Published token rule', { exact: true })).toBeVisible();
  await expect(page.getByText(/prompt_tokens >= 100/)).toBeVisible();
  const published = () =>
    sql(
      `SELECT i.price::text FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id JOIN price_books b ON b.id=v.price_book_id JOIN models m ON m.id=i.public_model_id WHERE b.is_default AND v.status='published' AND m.model_id='conduit-e2e-conditional' ORDER BY v.version DESC LIMIT 1`
    );
  const before = published();
  expect(JSON.parse(before)).toEqual(retailPrice);
  const openEditor = async () => {
    await page.goto('/models');
    // Follow the existing model-management tour through its real highlighted
    // settings control before interacting with catalog cards.
    const tour = page.locator('#driver-popover-content');
    const tourVisible = await tour
      .waitFor({ state: 'visible', timeout: 3000 })
      .then(() => true)
      .catch(() => false);
    if (tourVisible) {
      const completion = page.waitForResponse(
        (response) =>
          response.url().includes('/admin/graphql') &&
          (response.request().postData()?.includes('CompleteSystemModelSettingOnboarding') ?? false)
      );
      await page.locator('[data-settings-button].driver-active-element').click();
      const response = await completion;
      expect(response.ok()).toBe(true);
      expect((await response.json()).errors).toBeUndefined();
      await expect(page.locator('.driver-overlay')).not.toBeVisible();
      const settings = page.locator('[data-slot="dialog-content"]').filter({ hasText: /Model Settings|模型设置/i });
      await expect(settings).toBeVisible();
      await settings.locator('[data-slot="dialog-close"]').click();
      await expect(settings).not.toBeVisible();
    }
    await page.getByText('E2E conditional model', { exact: true }).first().click();
    await page.getByRole('button', { name: 'Set retail price', exact: true }).click();
    return page.getByRole('dialog', { name: '编辑零售价变更', exact: true });
  };
  let editor = await openEditor();
  const ruleLabel = editor.locator('input[aria-label="Rule label"]');
  const groupCard = editor.locator('details:has(input[aria-label="Rule label"])').first();
  const ruleCard = editor.locator('details:has(input[aria-label="Rule label"])').last();
  const groupTitle = groupCard.locator(':scope > summary');
  const ruleTitle = ruleCard.locator(':scope > summary');
  await expect(ruleTitle).toHaveText('Published token rule');
  await ruleLabel.click();
  await expect(ruleCard).toHaveAttribute('open', '');
  await expect(groupCard).toHaveAttribute('open', '');
  await ruleTitle.click();
  await expect(ruleCard).not.toHaveAttribute('open', '');
  await expect(ruleLabel).not.toBeVisible();
  await ruleTitle.click();
  await expect(ruleLabel).toHaveValue('Published token rule');
  await expect(ruleCard.getByRole('textbox', { name: 'Time / token condition (text or AST JSON)', exact: true })).toHaveValue(
    'prompt_tokens >= 100'
  );
  await groupTitle.click();
  await expect(groupCard).not.toHaveAttribute('open', '');
  await expect(ruleTitle).not.toBeVisible();
  await groupTitle.click();
  await expect(ruleLabel).toHaveValue('Published token rule');
  await editor.getByRole('button', { name: 'Document JSON', exact: true }).click();
  expect(JSON.parse(await editor.getByRole('textbox', { name: 'Document JSON', exact: true }).inputValue())).toEqual(retailPrice);
  const draft = structuredClone(retailPrice!);
  draft.global!.items[0].pricing.usagePerUnit = '0';
  draft.groups[0].base.items[0].pricing.usagePerUnit = '1';
  draft.groups[0].rules[0].condition = 'prompt_tokens >= 500 && minute >= 0';
  draft.groups[0].rules[0].label = 'Unpublished draft rule';
  await editor.getByRole('textbox', { name: 'Document JSON', exact: true }).fill(JSON.stringify(draft));
  const saveResponse = page.waitForResponse(
    (response) =>
      response.url().includes('/admin/graphql') &&
      (response.request().postData()?.includes('mutation SaveRetailPriceChangeSetItem') ?? false)
  );
  await editor.getByRole('button', { name: '保存变更', exact: true }).click();
  const saved = await saveResponse;
  expect(saved.ok()).toBe(true);
  const savedPayload = await saved.json();
  expect(savedPayload.errors).toBeUndefined();
  expect(savedPayload.data.saveRetailPriceChangeSetItem.item.validationError).toBeNull();
  await expect(editor).not.toBeVisible();
  expect(published()).toBe(before);
  await page.reload();
  editor = await openEditor();
  await editor.getByRole('button', { name: 'Document JSON', exact: true }).click();
  const reopened = await editor.getByRole('textbox', { name: 'Document JSON', exact: true }).inputValue();
  expect(JSON.parse(reopened)).toEqual(draft);
  await editor.getByRole('button', { name: '取消', exact: true }).click();
  expect(published()).toBe(before);

  await page.goto('/project/wallet');
  await expect(page.getByText('Credits 1000', { exact: true }).first()).toBeVisible();
  await page.goto('/project/playground');
  await page.getByLabel('API key', { exact: true }).fill(apiKey);
  const modelResponse = page.waitForResponse((response) => response.url().endsWith('/v1/models'));
  await page.getByRole('button', { name: 'Load available models', exact: true }).click();
  expect((await modelResponse).status()).toBe(200);
  await expect(page.getByRole('combobox')).toHaveValue('conduit-e2e-conditional');
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
  await expect(page.getByTestId('requests-table')).toContainText('conduit-e2e-conditional');
  await page.goto('/project/wallet');
  await expect(page.getByText('Credits 995.8', { exact: true }).first()).toBeVisible();
  // Database and browser assertions deliberately never print or attach the API key.
});

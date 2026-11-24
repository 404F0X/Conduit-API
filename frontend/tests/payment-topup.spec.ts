import { test, expect } from '@playwright/test';
import { gotoAndEnsureAuth } from './auth.utils';
import { sql, seedBillingFixture, createBillingProject, selectBillingModelGroup } from './billing-fixture';

// Keys and signed cashier URLs must never enter trace/screenshot/video artifacts.
test.use({ trace: 'off', screenshot: 'off', video: 'off' });
test.describe.configure({ retries: 0 });

for (const [suffix, channel] of [
  ['paymentsa', 'ALIPAY'],
  ['paymentsw', 'WECHAT'],
] as const) {
  test(`local reference ${channel}: frozen order → verified credit → normal-key SSE debit`, async ({ page, context }) => {
    test.setTimeout(120000);
    if (process.env.CONDUIT_E2E_ISOLATED !== '1' || !process.env.CONDUIT_E2E_PAYMENT_SPKI)
      throw new Error('Use the isolated HTTPS payment harness');
    await gotoAndEnsureAuth(page, '/project/api-keys');
    const projectID = createBillingProject(suffix);
    await page.evaluate((id) => localStorage.setItem('conduit_selected_project_id', `gid://conduit/Project/${id}`), projectID);
    const { groupID, modelKey } = seedBillingFixture(projectID, { suffix, rechargePPM: '1500000' });
    const balance = () =>
      BigInt(sql(`SELECT credit_balance_micros FROM project_wallets WHERE project_id=${projectID} AND currency='STATION_CREDIT'`));
    const initialBalance = balance();
    await page.goto('/project/wallet');
    await page.reload();
    await page.getByLabel('Amount to pay (CNY)', { exact: true }).fill('0.01');
    await page.getByLabel('Payment method').selectOption(channel);
    await page.getByRole('button', { name: 'Create order and review', exact: true }).click();
    const detail = page.getByRole('region', { name: 'Confirm payment order', exact: true });
    await expect(detail).toContainText('CNY 0.01');
    await expect(detail).toContainText('Credits 150');
    await expect(detail).toContainText('1.5×');
    const orderID = (await detail.locator('dd').first().textContent())?.trim();
    if (!orderID || !/^[a-f\d]{8}(?:-[a-f\d]{4}){3}-[a-f\d]{12}$/i.test(orderID)) throw new Error('Expected server order identity');
    expect(balance()).toBe(initialBalance);
    await page.getByRole('button', { name: 'Verify with payment provider', exact: true }).click();
    await expect(detail).toContainText('Awaiting verified payment');
    expect(balance()).toBe(initialBalance);
    const popupReady = context.waitForEvent('page');
    await detail.getByRole('link', { name: 'Open cashier', exact: true }).click();
    const cashier = await popupReady;
    await expect(cashier.getByRole('heading', { name: 'Local reference cashier' })).toBeVisible();
    await cashier.getByRole('link', { name: 'Return without paying', exact: true }).click();
    await expect(cashier).toHaveURL(/\/project\/wallet$/);
    expect(sql(`SELECT status FROM payment_orders WHERE id='${orderID}'`)).toBe('pending');
    expect(balance()).toBe(initialBalance);
    await cashier.goBack();
    await cashier.getByRole('button', { name: 'Pay fixture order', exact: true }).click();
    await expect(cashier.getByRole('heading', { name: 'Fixture notification accepted' })).toBeVisible();
    await cashier.getByRole('link', { name: 'Return to wallet' }).click();
    await expect(cashier).toHaveURL(/\/project\/wallet$/);
    await cashier.close();
    await page.bringToFront();
    await page.getByRole('button', { name: 'Verify with payment provider', exact: true }).click();
    await expect(detail).toContainText('Credited to wallet');
    await page.getByRole('button', { name: 'Verify with payment provider', exact: true }).click();
    await expect.poll(() => balance().toString(), { timeout: 30000 }).toBe((initialBalance + 150000000n).toString());
    expect(
      sql(
        `SELECT count(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup' AND reference_type='payment_order' AND reference_id='${orderID}' AND amount_micros=150000000 AND idempotency_key='payment-topup:${orderID}'`
      )
    ).toBe('1');
    await page.goto('/project/api-keys');
    await page.getByRole('button', { name: /Create API Key|创建 API Key/i }).click();
    const createDialog = page.getByRole('dialog');
    await createDialog.getByLabel(/Name|名称/i, { exact: true }).fill(`E2E payment ${suffix} browser key`);
    await selectBillingModelGroup(page, createDialog, groupID);
    await createDialog.getByRole('button', { name: /Create|创建/i, exact: true }).click();
    const view = page.getByRole('dialog', { name: /View API Key|查看 API 密钥/i });
    await expect(view).toBeVisible();
    const code = view.locator('code').first();
    await code.locator('..').getByRole('button').first().click();
    const apiKey = (await code.textContent())?.trim();
    if (!apiKey || apiKey.includes('*')) throw new Error('Newly created key was not revealed');
    const keyID = Number(sql(`SELECT id FROM api_keys WHERE name='E2E payment ${suffix} browser key' AND type='user' AND deleted_at=0`));
    expect(Number.isSafeInteger(keyID) && keyID > 0).toBe(true);

    await page.goto('/project/playground');
    await page.getByLabel('API key', { exact: true }).fill(apiKey);
    const modelResponse = page.waitForResponse((response) => response.url().endsWith('/v1/models'));
    await page.getByRole('button', { name: 'Load available models', exact: true }).click();
    expect((await modelResponse).status()).toBe(200);
    await expect(page.getByRole('combobox')).toHaveValue(modelKey);
    await page.getByLabel('Max Tokens', { exact: true }).fill('32');
    await page.getByPlaceholder('Type a message...').fill('deterministic fixture question');
    const completion = page.waitForResponse((response) => response.url().endsWith('/v1/chat/completions'));
    await page.getByRole('button', { name: 'Submit', exact: true }).click();
    const stream = await completion;
    expect(stream.status()).toBe(200);
    expect(stream.headers()['content-type']).toContain('text/event-stream');
    await expect(page.getByText('local streaming billing response', { exact: true })).toBeVisible();

    await expect
      .poll(
        () =>
          sql(
            `SELECT count(*) FROM customer_charge_events c JOIN usage_logs u ON u.id=c.usage_log_id JOIN charge_settlements s ON s.charge_event_id=c.id WHERE u.api_key_id=${keyID} AND s.status='settled'`
          ),
        { timeout: 30000, intervals: [200, 500, 1000] }
      )
      .toBe('1');
    expect(sql(`SELECT count(*) FROM usage_logs WHERE api_key_id=${keyID} AND prompt_tokens=100 AND completion_tokens=20`)).toBe('1');
    expect(sql(`SELECT count(*) FROM usage_logs WHERE api_key_id=${keyID}`)).toBe('1');
    expect(sql(`SELECT count(*) FROM customer_charge_events c JOIN usage_logs u ON u.id=c.usage_log_id WHERE u.api_key_id=${keyID}`)).toBe(
      '1'
    );
    expect(
      sql(
        `SELECT count(*)||':'||(-sum(l.amount_micros)) FROM project_credit_ledger_entries l JOIN usage_logs u ON l.reference_type='usage_log' AND l.reference_id=u.id::text WHERE u.api_key_id=${keyID} AND l.entry_type='usage_charge'`
      )
    ).toBe('1:4200000');
    await expect.poll(() => balance().toString(), { timeout: 30000 }).toBe((initialBalance + 150000000n - 4200000n).toString());
    expect(sql(`SELECT credit_reserved_micros FROM project_wallets WHERE project_id=${projectID} AND currency='STATION_CREDIT'`)).toBe('0');
    expect(
      sql(
        `SELECT count(*) FROM project_wallet_reservations r JOIN project_wallets w ON w.id=r.wallet_id WHERE w.project_id=${projectID} AND r.status IN ('reserved','shadow_reserved','capturing','soft_insufficient')`
      )
    ).toBe('0');
  });
}

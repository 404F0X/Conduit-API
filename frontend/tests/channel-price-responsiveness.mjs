import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { chromium, expect } from '@playwright/test';

// The caller serves the real build with a task-owned GraphQL fixture. Never use production.
const origin = process.env.CONDUIT_PRICE_TEST_ORIGIN ?? 'http://127.0.0.1:18478';
const output = resolve(process.env.CONDUIT_PRICE_TEST_OUTPUT ?? '../.codex-workflow/runs/price-apply-freeze-20261008/artifacts/price-ui/checks');
await mkdir(output, { recursive: true });
const models = Array.from({ length: 103 }, (_, index) => `model-${String(index + 1).padStart(3, '0')}`);
const results = [], requests = [], errors = [], consoleErrors = [], timings = [];
const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
await context.route('**/*', async (route) => {
  const url = new URL(route.request().url());
  if (url.origin === origin || url.protocol === 'data:') return route.continue();
  if (url.hostname === 'raw.githubusercontent.com') return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ providers: { openai: { id: 'openai', name: 'Synthetic Provider', models: models.map((id) => ({ id, cost: { input: 1, output: 2 } })) } } }) });
  if (url.hostname === 'fonts.googleapis.com') return route.fulfill({ contentType: 'text/css', body: '' });
  return route.abort();
});
await context.addInitScript(() => {
  window.__priceMetrics = { beats: 0, maxGap: 0, longTasks: [] };
  let previous = performance.now();
  setInterval(() => { const now = performance.now(); window.__priceMetrics.beats++; window.__priceMetrics.maxGap = Math.max(window.__priceMetrics.maxGap, now - previous); previous = now; }, 25);
  new PerformanceObserver((list) => window.__priceMetrics.longTasks.push(...list.getEntries().map((entry) => ({ start: entry.startTime, duration: entry.duration })))).observe({ entryTypes: ['longtask'] });
});
const page = await context.newPage();
page.setDefaultTimeout(10000);
page.on('pageerror', (error) => errors.push(String(error)));
page.on('console', (message) => { if (message.type() === 'error') consoleErrors.push(message.text()); });
page.on('request', (request) => {
  if (request.method() === 'POST' && request.url() === `${origin}/admin/graphql`) requests.push(request.postDataJSON());
});
const check = async (id, run) => {
  if (process.env.CONDUIT_PRICE_TEST_CASE && !id.startsWith(process.env.CONDUIT_PRICE_TEST_CASE)) return;
  try { await run(); results.push({ id, status: 'PASS' }); }
  catch (error) { results.push({ id, status: 'FAIL', error: String(error) }); await writeFile(resolve(output, `${id}-failure.txt`), await page.locator('body').innerText()); }
  await page.screenshot({ path: resolve(output, `${id}.png`) });
};
const measure = async (action, run) => {
  await page.evaluate(() => { window.__priceMetrics.maxGap = 0; window.__priceMetrics.longTasks = []; });
  const before = await page.evaluate(() => window.__priceMetrics.beats), start = Date.now();
  await run();
  await page.waitForFunction((beats) => window.__priceMetrics.beats > beats, before);
  const metrics = await page.evaluate(() => ({ ...window.__priceMetrics, inputs: document.querySelectorAll('[role=dialog] input').length }));
  timings.push({ action, wallMs: Date.now() - start, ...metrics });
  assert.ok(metrics.maxGap < 1000 && metrics.longTasks.every((task) => task.duration < 1000), `${action}: main thread stalled`);
};
const open = async () => { await page.goto(`${origin}/__repair_bootstrap?target=%2Fchannels`); await expect(page.locator('table')).toContainText('Synthetic NEWAPI'); };
const priceDialog = () => page.getByRole('dialog').filter({ has: page.getByRole('heading', { name: 'Upstream Purchase Prices', exact: true }) });
const openPrices = async () => { await page.getByTestId('row-actions').click(); await page.getByRole('menuitem', { name: 'Model Price', exact: true }).click(); await expect(priceDialog()).toBeVisible(); await expect(priceDialog().getByRole('button', { name: 'Create Draft', exact: true })).toBeEnabled(); };
const probe = async () => { await priceDialog().getByRole('button', { name: 'Probe actual prices', exact: true }).click(); await expect(priceDialog().getByRole('button', { name: 'Apply 103 prices to draft', exact: true })).toBeEnabled(); };
const apply = async (label) => measure(label, async () => { await priceDialog().getByRole('button', { name: 'Apply 103 prices to draft', exact: true }).click(); await expect(priceDialog().getByTestId('channel-price-card').first()).toBeVisible(); });
const closePrices = async (label) => measure(label, async () => { await priceDialog().getByRole('button', { name: 'Cancel', exact: true }).click(); await expect(page.getByRole('dialog')).toHaveCount(0); });
const seek = async (index) => {
  const scroller = page.getByTestId('model-price-dialog-body');
  if (index >= 102) {
    // Natural end scrolling also exercises remeasurement of the last virtual rows.
    for (let attempt = 0; attempt < 4; attempt++) {
      await scroller.evaluate((el) => { el.scrollTop = el.scrollHeight; });
      await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    }
  }
  for (let attempt = 0; attempt < 30; attempt++) {
    const card = page.locator(`[data-testid="channel-price-card"][data-price-index="${index}"]`);
    if (await card.count()) { await card.scrollIntoViewIfNeeded(); return card; }
    const range = await priceDialog().getByTestId('channel-price-card').evaluateAll((cards) => cards.map((el) => Number(el.dataset.priceIndex)));
    const nearest = range.length ? (Math.max(...range) < index ? Math.max(...range) : Math.min(...range)) : 0;
    await scroller.evaluate((el, delta) => { el.scrollTop += delta; }, (index - nearest) * 400);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  }
  throw new Error(`Price ${index} not reachable by scrolling`);
};
const field = (name) => priceDialog().locator(`input[name="${name}"]`);
try {
  for (const target of ['same-edit', 'other-edit', 'price']) {
    await check(`P00-quick-reopen-${target}`, async () => {
      await page.request.get(`${origin}/__repair_control?second_channel=1`);
      await open();
      const edit = (row) => page.locator(`table tbody tr:nth-child(${row}) td:last-child button:nth-last-child(3)`);
      await edit(1).click();
      await expect(page.getByRole('heading', { name: 'Edit Channel', exact: true })).toBeVisible();
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
      if (target === 'price') {
        await page.getByTestId('row-actions').first().click();
        await page.getByRole('menuitem', { name: 'Model Price', exact: true }).click();
        await expect(priceDialog()).toBeVisible();
      } else {
        await edit(target === 'same-edit' ? 1 : 2).click();
        await expect(page.getByRole('heading', { name: 'Edit Channel', exact: true })).toBeVisible();
      }
      await page.waitForTimeout(650);
      await expect(page.getByRole('dialog')).toBeVisible();
      if (target !== 'price') await expect(page.getByRole('dialog').locator('input[name="name"]')).toHaveValue(target === 'same-edit' ? 'Synthetic NEWAPI' : 'Synthetic SECOND');
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await page.request.get(`${origin}/__repair_control?second_channel=0`);
    });
  }
  await check('P01-table-interactions', async () => {
    await page.request.get(`${origin}/__repair_control?second_channel=0`);
    await open();
    for (let iteration = 0; iteration < 2; iteration++) {
      await measure('table-search', async () => { await page.getByPlaceholder('Search by name').fill('Synthetic'); await expect(page.getByPlaceholder('Search by name')).toHaveValue('Synthetic'); await page.getByPlaceholder('Search by name').fill(''); });
      await measure('table-expand', async () => { await page.locator('table tbody tr:first-child td:first-child button').click(); await expect(page.locator('table tbody')).toContainText('Basic'); await page.locator('table tbody tr:first-child td:first-child button').click(); });
      await measure('table-menu', async () => { await page.getByTestId('row-actions').click(); await expect(page.getByRole('menuitem', { name: 'Model Price', exact: true })).toBeVisible(); await page.keyboard.press('Escape'); });
      await measure('table-row-selection', async () => { const checkbox = page.locator('table tbody tr:first-child').getByRole('checkbox'); await checkbox.check(); await expect(checkbox).toBeChecked(); await checkbox.uncheck(); await expect(checkbox).not.toBeChecked(); });
      await measure('table-status-open-close', async () => { await page.getByTestId('channel-status-switch').click(); await expect(page.getByRole('heading', { name: 'Disable Channel', exact: true })).toBeVisible(); await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click(); await expect(page.getByRole('alertdialog')).toHaveCount(0); });
      await measure('table-edit-open-close', async () => { await page.locator('table tbody tr:first-child td:last-child button:nth-last-child(3)').click(); await expect(page.getByRole('heading', { name: 'Edit Channel', exact: true })).toBeVisible(); await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click(); await expect(page.getByRole('dialog')).toHaveCount(0); });
    }
  });
  await check('P02-existing-currency-and-advanced-values', async () => {
    await page.request.get(`${origin}/__repair_control?price_base=preserve`);
    try {
    await open(); await openPrices(); await probe(); await apply('apply-with-existing-values'); await apply('repeat-apply-with-existing-values');
    await seek(0);
    await expect(field('prices.0.price.items.0.pricing.usageTiered.tiers.0.pricePerUnit')).toHaveValue('7.125');
    await expect(field('prices.0.price.items.0.pricing.usageTiered.tiers.1.pricePerUnit')).toHaveValue('8.25');
    await expect(field('prices.0.price.items.3.promptWriteCacheVariants.0.pricing.usagePerUnit')).toHaveValue('1.23');
    await expect(field('prices.0.price.items.3.promptWriteCacheVariants.1.pricing.flatFee')).toHaveValue('0.45');
    await expect(page.locator('[data-price-index="0"]')).toContainText('CNY');
    await seek(1);
    await expect(field('prices.1.price.items.0.pricing.usagePerUnit')).toHaveValue('7.875');
    await seek(103); await seek(0);
    await expect(field('prices.0.price.items.3.promptWriteCacheVariants.0.pricing.usagePerUnit')).toHaveValue('1.23');
    await expect(field('prices.0.price.items.3.promptWriteCacheVariants.1.pricing.flatFee')).toHaveValue('0.45');
    await expect(field('prices.0.price.items.0.pricing.usageTiered.tiers.0.pricePerUnit')).toHaveValue('7.125');
    await expect(field('prices.0.price.items.0.pricing.usageTiered.tiers.1.pricePerUnit')).toHaveValue('8.25');
    await expect(page.locator('[data-price-index="0"]')).toContainText('CNY');
    await closePrices('cancel-preserved-values');
    } finally { await page.request.get(`${origin}/__repair_control?price_base=empty`); }
  });
  await check('P03-complete-103-draft-after-remount', async () => {
    await page.request.get(`${origin}/__repair_control?price_base=empty&second_channel=0`);
    await open(); await openPrices(); await probe();
    const createsBefore = requests.filter((request) => request.operationName === 'CreateProviderPriceChangeSet').length;
    await apply('apply-103'); await apply('repeat-apply-103');
    assert.equal(requests.filter((request) => request.operationName === 'CreateProviderPriceChangeSet').length, createsBefore, 'Apply must stay local');
    await closePrices('cancel-103');
    await openPrices(); await expect(priceDialog().getByTestId('channel-price-card')).toHaveCount(0);
    await probe(); await apply('reopen-apply-103');
    for (const [index, value, suffix] of [[0, '777.125', 'usagePerUnit'], [51, '888.25', 'usagePerUnit'], [102, '0.75', 'flatFee']]) {
      await seek(index); await measure(`edit-row-${index}`, async () => { await field(`prices.${index}.price.items.0.pricing.${suffix}`).fill(value); });
    }
    await seek(0); await expect(field('prices.0.price.items.0.pricing.usagePerUnit')).toHaveValue('777.125');
    await seek(51); await expect(field('prices.51.price.items.0.pricing.usagePerUnit')).toHaveValue('888.25');
    await page.setViewportSize({ width: 900, height: 700 });
    await seek(102); await expect(field('prices.102.price.items.0.pricing.flatFee')).toHaveValue('0.75');
    await field('prices.102.price.items.0.pricing.flatFee').scrollIntoViewIfNeeded();
    assert.ok(await field('prices.102.price.items.0.pricing.flatFee').evaluate((el) => {
      const field = el.getBoundingClientRect(), body = document.querySelector('[data-testid="model-price-dialog-body"]').getBoundingClientRect();
      return field.top >= body.top && field.bottom <= body.bottom;
    }), 'Last price input must be fully visible inside the actual scroll area');
    await page.screenshot({ path: resolve(output, 'P03-final-row-small-window.png') });
    const submitted = page.waitForRequest((request) => request.method() === 'POST' && request.postDataJSON()?.operationName === 'CreateProviderPriceChangeSet');
    await priceDialog().getByRole('button', { name: 'Create Draft', exact: true }).click();
    const variables = (await submitted).postDataJSON().variables;
    assert.equal(variables.input.length, 103);
    assert.equal(new Set(variables.input.map((row) => row.modelId)).size, 103);
    for (const [index, row] of variables.input.entries()) {
      assert.equal(row.modelId, models[index]); assert.equal(row.currencyCode, 'USD');
      if (index === 102) { assert.equal(row.price.items.length, 1); assert.equal(row.price.items[0].pricing.mode, 'flat_fee'); assert.equal(row.price.items[0].pricing.flatFee, '0.75'); }
      else {
        assert.deepEqual(row.price.items.map((item) => item.itemCode), ['prompt_tokens', 'completion_tokens', 'prompt_cached_tokens', 'prompt_write_cached_tokens']);
        assert.ok(row.price.items.every((item) => item.pricing.mode === 'usage_per_unit'));
        assert.deepEqual(row.price.items.map((item) => item.pricing.usagePerUnit), [index === 0 ? '777.125' : index === 51 ? '888.25' : `${index + 1}.125`, `${(index + 1) * 2}.25`, '0', `${index + 1}.5`]);
      }
    }
    await writeFile(resolve(output, 'complete-103-draft-payload.json'), JSON.stringify(variables, null, 2));
    await expect(page).toHaveURL(/\/change-sets\/401/);
    assert.equal(requests.filter((request) => request.operationName === 'CreateProviderPriceChangeSet').length, createsBefore + 1);
    assert.equal(errors.length, 0); assert.equal(consoleErrors.length, 0);
  });
} finally { await context.close(); await browser.close(); }
await writeFile(resolve(output, 'browser-checks.json'), JSON.stringify({ results, timings, errors, consoleErrors, requestCounts: Object.fromEntries([...new Set(requests.map((request) => request.operationName))].map((name) => [name, requests.filter((request) => request.operationName === name).length])), syntheticOnly: true, browserClosed: true }, null, 2));
console.log(JSON.stringify({ results, timings, errors, consoleErrors }, null, 2));
process.exitCode = results.some((result) => result.status === 'FAIL') ? 1 : 0;

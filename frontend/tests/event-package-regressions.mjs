import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { chromium, expect } from '@playwright/test';

// The caller starts the task-owned static build/GraphQL fixture. No production API is used.
const origin = process.env.CONDUIT_EVENT_REGRESSION_ORIGIN ?? 'http://127.0.0.1:18464';
const output = resolve(process.env.CONDUIT_EVENT_REGRESSION_OUTPUT ?? '../.codex-workflow/runs/quality-repair-20261006/artifacts');
const readabilityOnly = process.env.CONDUIT_EVENT_REGRESSION_SUITE === 'readability';
const prefix = readabilityOnly ? 'readable-event-ui' : 'draft-implement-r3-a2';
await mkdir(output, { recursive: true });
const results = [];
const blocked = [];
const boundaries = [];
const browser = await chromium.launch({ headless: true });
try {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await context.route('**/*', (route) => {
    const url = new URL(route.request().url());
    if (url.origin === origin || url.protocol === 'data:') return route.continue();
    blocked.push(url.origin);
    return route.abort();
  });
  const page = await context.newPage();
  page.setDefaultTimeout(12000);
  const check = async (id, fn) => {
    if (readabilityOnly && !id.startsWith('R')) return;
    try {
      await fn();
      results.push({ id, status: 'PASS' });
    } catch (error) {
      results.push({ id, status: 'FAIL', error: String(error) });
      await writeFile(resolve(output, `${prefix}-${id}-failure.txt`), await page.locator('body').innerText());
    }
    await page.screenshot({ path: resolve(output, `${prefix}-${id}.png`), fullPage: true });
  };
  const open = async (path, language = 'en') => {
    await page.goto(`${origin}/__repair_bootstrap?target=${encodeURIComponent(path)}&language=${language}`);
    await expect(page.locator('main')).toBeVisible();
  };
  await check('F01-F07-review-recovery', async () => {
    await open('/change-sets/101');
    const approve = page.getByRole('button', { name: 'Approve and apply entire package', exact: true });
    await approve.click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox', { name: 'Review note (optional)' }).fill('Keep this review note after the lost response');
    await dialog.getByRole('button', { name: 'Approve and apply entire package', exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.locator('main')).toContainText('Applied');
    await expect(page.locator('main')).toContainText('Keep this review note after the lost response');
    await expect(approve).toHaveCount(0);
  });
  await check('F03-tiered', async () => {
    await open('/change-sets/122');
    await page.getByRole('tab', { name: 'Changes', exact: true }).click();
    await page.getByRole('button', { name: /Synthetic model 1/ }).first().click();
    const panel = page.getByRole('tabpanel');
    await expect(panel).toContainText('Marginal tiered price');
    await expect(panel).toContainText('Up to 1000000 tokens');
    await expect(panel).toContainText('Remaining usage (no upper limit)');
    for (const price of ['10', '15', '20', '25']) await expect(panel.locator('li').getByText(price, { exact: true }).first()).toBeVisible();
    await panel.getByText('Technical evidence', { exact: true }).click();
    await expect(panel.locator('pre')).toContainText('"before"');
    await expect(panel.locator('pre')).toContainText('"proposed"');
    await expect(panel.locator('pre')).toContainText('"usageTiered"');
  });
  await check('F03-cache-variant', async () => {
    await open('/change-sets/123');
    await page.getByRole('tab', { name: 'Changes', exact: true }).click();
    await page.getByRole('button', { name: /Synthetic model 1/ }).first().click();
    const panel = page.getByRole('tabpanel');
    await expect(panel.getByText('5-minute cache', { exact: true }).first()).toBeVisible();
    await expect(panel.locator('dd').getByText('2', { exact: true }).first()).toBeVisible();
    await expect(panel.locator('dd').getByText('3', { exact: true }).first()).toBeVisible();
  });
  await check('F04-audit', async () => {
    await open('/change-sets/102');
    await page.getByRole('tab', { name: 'Activity', exact: true }).click();
    for (const id of [10, 11, 12]) await expect(page.getByRole('tabpanel')).toContainText(`gid://conduit/User/${id}`);
    assert.equal(await page.getByRole('tabpanel').locator('time[datetime]').count(), 3);
  });
  await check('F06-empty-recovery', async () => {
    await open('/change-sets');
    await page.getByRole('textbox', { name: 'Search packages, models or channels' }).fill('no-match-a2');
    await expect(page.getByRole('heading', { name: 'No packages match this view' })).toBeVisible();
    await page.getByRole('button', { name: 'Clear filters and view all history' }).click();
    await expect(page.getByRole('heading', { name: 'No packages match this view' })).toHaveCount(0);
    await expect(page.locator('main')).toContainText('Synthetic event package');
    await page.request.get(`${origin}/__repair_control?empty=1`);
    await open('/change-sets');
    await expect(page.getByRole('heading', { name: 'No event packages yet' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Clear filters and view all history' })).toHaveCount(0);
    await page.request.get(`${origin}/__repair_control?empty=0`);
  });
  await check('F02-mobile', async () => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open('/channels');
    await page.evaluate(() => localStorage.setItem('conduit:channel-discovery:1', 'a2-mobile-op'));
    await page.reload();
    const card = page.getByTestId('mobile-channel-card');
    await expect(card.getByRole('button', { name: 'Discover now', exact: true })).toBeVisible();
    await expect(card.getByRole('link', { name: 'Event packages', exact: true })).toBeVisible();
    const result = card.getByRole('button', { name: 'Discovery result', exact: true });
    await result.focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('dialog')).toBeVisible();
    await page.keyboard.press('Escape');
    await page.request.get(`${origin}/__repair_control?commercial=0`);
    await open('/channels');
    await expect(card.getByRole('button', { name: 'Discover now', exact: true })).toBeVisible();
    await expect(card.getByRole('link', { name: 'Event packages', exact: true })).toHaveCount(0);
    await page.request.get(`${origin}/__repair_control?commercial=1`);
  });
  await check('F05-saved-inflight', async () => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await open('/channels');
    await page.getByRole('button', { name: 'New NEWAPI channel', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox', { name: 'Channel name', exact: true }).fill('A2 saved channel');
    await dialog.getByRole('textbox', { name: 'Upstream console / API URL', exact: true }).fill('https://example.test/gateway/v1/');
    await dialog.getByLabel('API key', { exact: true }).fill('synthetic-only-not-a-credential');
    const started = page.waitForRequest((r) => r.method() === 'POST' && r.postDataJSON()?.operationName === 'InitializeNewApiChannel');
    await dialog.getByRole('button', { name: 'Save and discover', exact: true }).click();
    await started;
    await expect(dialog.getByRole('heading', { name: 'Channel saved', exact: true })).toBeVisible({ timeout: 2000 });
    await expect(dialog.getByRole('link', { name: 'View saved channel', exact: true })).toBeVisible();
    await expect(dialog).toContainText('Operation identity');
    for (const name of ['Model discovery', 'Price discovery', 'Package validation']) await expect(dialog.getByRole('heading', { name, exact: true })).toBeVisible();
    await expect(dialog).toContainText('Initialization request in progress');
    const startButton = dialog.getByRole('button', { name: 'Start model and price discovery', exact: true });
    if (await startButton.count()) await expect(startButton).toBeDisabled();
    await expect(dialog).toContainText('Partially completed', { timeout: 10000 });
    await expect(dialog).toContainText('103');
    await expect(dialog).toContainText('Synthetic price stage timed out');
  });
  if (readabilityOnly) {
    await check('R01-item-counts-and-pagination', async () => {
      for (const language of ['zh-CN', 'en']) {
        await page.setViewportSize({ width: 1280, height: 900 });
        await open('/changelog', language);
        const single = page.locator('main a[href*="/change-sets/101"]');
        const mixed = page.locator('main a[href*="/change-sets/102"]');
        await expect(single).toContainText(language === 'en' ? '1 item updated' : '修改 1 项');
        assert.doesNotMatch(await single.innerText(), /\+0|Δ|−0|![0-9]/);
        assert.doesNotMatch(await single.innerText(), language === 'en' ? /added|deleted|need[s]? action/ : /新增|删除|需处理/);
        for (const label of language === 'en' ? ['2 items added', '3 items updated', '4 items deleted'] : ['新增 2 项', '修改 3 项', '删除 4 项']) {
          await expect(mixed).toContainText(label);
        }
        await page.getByRole('button', { name: language === 'en' ? 'Next' : '下一页', exact: true }).click();
        await expect(page.locator('main a[href*="/change-sets/125"]')).toContainText(language === 'en' ? '1 item needs action' : '需处理 1 项');
        assert.equal(await page.locator('main a[href*="/change-sets/"]').count(), 5);
        await page.getByRole('button', { name: language === 'en' ? 'Previous' : '上一页', exact: true }).click();
        await expect(page.locator('main a[href*="/change-sets/120"]')).toHaveCount(1);
        assert.equal(await page.locator('main a[href*="/change-sets/"]').count(), 20);
        await page.getByRole('tab', { name: language === 'en' ? 'Pending review 23' : '待审核 23', exact: true }).click();
        await expect(page.locator('main')).toContainText(language === 'en' ? '23 Event packages' : '23 事件包');
        await page.getByRole('tab', { name: language === 'en' ? 'Needs action 1' : '需处理 1', exact: true }).click();
        await expect(page.locator('main a[href*="/change-sets/125"]')).toHaveCount(1);
        assert.equal(await page.locator('main a[href*="/change-sets/"]').count(), 1);
      }
    });
    await check('R02-complete-scroll-boundaries', async () => {
      const cases = [
        { name: 'desktop', width: 2048, height: 1139 },
        { name: 'desktop-normal', width: 1280, height: 900 },
        { name: 'small-window', width: 1024, height: 640 },
        { name: 'text-125', width: 1280, height: 900, textZoom: 1.25 },
        { name: 'text-200', width: 1280, height: 900, textZoom: 2 },
        { name: 'page-125', width: 1280, height: 900, pageZoom: 1.25 },
        { name: 'page-200', width: 1280, height: 900, pageZoom: 2 },
        { name: 'mobile', width: 390, height: 844 },
      ];
      for (const route of ['/changelog', '/change-sets']) {
        for (const example of cases) {
          // Browser page zoom reflows at a proportionally smaller CSS viewport.
          await page.setViewportSize({ width: Math.round(example.width / (example.pageZoom ?? 1)), height: Math.round(example.height / (example.pageZoom ?? 1)) });
          await open(route, 'zh-CN');
          await expect(page.locator('main a[href*="/change-sets/120"]')).toHaveCount(1);
          if (example.textZoom) {
            // Text-only resizing preserves layout lengths while enlarging every rendered font/line-height.
            await page.evaluate((factor) => {
              const elements = [...document.querySelectorAll('body *')].map((el) => {
                const css = getComputedStyle(el);
                return { el, fontSize: parseFloat(css.fontSize), lineHeight: parseFloat(css.lineHeight) };
              });
              for (const { el, fontSize, lineHeight } of elements) {
                el.style.fontSize = `${fontSize * factor}px`;
                if (Number.isFinite(lineHeight)) el.style.lineHeight = `${lineHeight * factor}px`;
              }
            }, example.textZoom);
          }
          const boundary = await page.evaluate(async () => {
            const main = document.querySelector('main');
            main.scrollTop = main.scrollHeight;
            await new Promise((done) => requestAnimationFrame(done));
            const card = main.children[1], row = main.querySelector('a[href*="/change-sets/120"]'), footer = main.lastElementChild, header = main.previousElementSibling;
            const rect = (el) => { const r = el.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, height: r.height }; };
            return { viewport: { width: innerWidth, height: innerHeight }, header: rect(header), intro: rect(header.querySelector('p')), main: rect(main), card: rect(card), row: rect(row), title: rect(row.querySelector('h2')), footer: rect(footer), scrollTop: main.scrollTop, scrollHeight: main.scrollHeight, clientHeight: main.clientHeight, clientWidth: main.clientWidth, scrollWidth: main.scrollWidth, cardClientHeight: card.clientHeight, cardScrollHeight: card.scrollHeight, overflow: getComputedStyle(main).overflowY };
          });
          boundaries.push({ route, ...example, ...boundary });
          assert.ok(boundary.scrollTop > 0, `${example.name}: user can scroll`);
          assert.ok(boundary.header.bottom <= boundary.main.top + 1 && boundary.intro.bottom <= boundary.header.bottom + 1, `${example.name}: header does not cover the list`);
          assert.ok(['auto', 'scroll'].includes(boundary.overflow));
          assert.ok(boundary.cardScrollHeight <= boundary.cardClientHeight + 1, `${example.name}: no hidden card content`);
          assert.ok(boundary.row.top >= boundary.main.top - 1 && boundary.row.bottom <= boundary.main.bottom + 1, `${example.name}: complete final row in viewport`);
          assert.ok(boundary.title.top >= boundary.row.top && boundary.title.bottom <= boundary.row.bottom, `${example.name}: complete target title`);
          assert.ok(boundary.footer.top >= boundary.row.bottom && boundary.footer.bottom <= boundary.main.bottom + 1, `${example.name}: pagination after row and visible`);
          assert.ok(boundary.footer.bottom <= boundary.viewport.height + 1 && boundary.row.top >= 0, `${example.name}: viewport contains final row and footer`);
          assert.ok(boundary.scrollWidth <= boundary.clientWidth + 1, `${example.name}: no horizontal clipping`);
          await page.screenshot({ path: resolve(output, `${prefix}-${route.slice(1)}-${example.name}-bottom.png`) });
        }
      }
    });
    await check('R03-last-row-keyboard-entry', async () => {
      await page.setViewportSize({ width: 390, height: 844 });
      await open('/changelog', 'zh-CN');
      const last = page.locator('main a[href*="/change-sets/120"]');
      await last.focus();
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(/\/change-sets\/120/);
      await expect(page.locator('main')).toContainText('Final channel with a long name to review completely');
    });
  }
  await context.close();
} finally {
  await browser.close();
}
await writeFile(resolve(output, `${prefix}-browser-checks.json`), JSON.stringify({ checks: results, boundaries, blocked_external_origins: [...new Set(blocked)], browser_closed: true, synthetic_only: true }, null, 2));
console.log(JSON.stringify(results, null, 2));
process.exitCode = results.some((r) => r.status === 'FAIL') ? 1 : 0;

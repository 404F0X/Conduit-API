import { expect, type Locator, type Page } from '@playwright/test';

export async function enterChannelConfiguration(dialog: Locator, provider = 'openai') {
  await dialog.getByTestId(`provider-${provider}`).click();
  await dialog.getByRole('button', { name: /^(Next|下一步)$/ }).click();
  await expect(dialog.getByRole('tab', { name: /^(Connection & models|连接与模型)$/ })).toBeVisible();
  await expect(dialog.getByTestId('channel-name-input')).toBeVisible();
}

export async function setChannelBilling(dialog: Locator) {
  await dialog.getByRole('tab', { name: /^(Quota & billing|额度与计费)$/ }).click();
  await dialog.locator('#channel-billing-currency').fill('CNY');
  await dialog.locator('#channel-recharge-multiplier').fill('1');
  await dialog.getByRole('tab', { name: /^(Connection & models|连接与模型)$/ }).click();
  await expect(dialog.getByTestId('channel-name-input')).toBeVisible();
}

export async function addChannelTag(dialog: Locator, tag: string) {
  await dialog.getByRole('tab', { name: /^(Other settings|其他设置)$/ }).click();
  const input = dialog.locator('[data-channel-field="tags"] input');
  await input.fill(tag);
  await input.press('Enter');
  await expect(dialog.getByRole('button', { name: `Remove ${tag} tag`, exact: true })).toBeVisible();
  await dialog.getByRole('tab', { name: /^(Connection & models|连接与模型)$/ }).click();
}

// This fixture uses the same visible form and server mutation as the CRUD test.
// A status-filter test must not depend on another case having created a row.
export async function createDisabledChannelFixture(page: Page, name: string, baseURL: string) {
  await page.getByTestId('add-channel-button').click();
  const dialog = page.getByRole('dialog');
  await enterChannelConfiguration(dialog);
  await dialog.getByTestId('channel-name-input').fill(name);
  await dialog.getByTestId('channel-base-url-input').fill(baseURL);
  await dialog.getByTestId('channel-api-key-input').fill('sk-isolated-channel-status-fixture');
  await dialog.getByTestId('quick-model-gpt-4o').click();
  await dialog.getByTestId('add-selected-models-button').click();
  await dialog.getByTestId('default-test-model-select').click();
  await page.getByRole('option', { name: 'gpt-4o', exact: true }).click();
  await setChannelBilling(dialog);
  const creation = page.waitForResponse(
    (response) => response.url().includes('/admin/graphql') && (response.request().postData()?.includes('createChannel(') ?? false)
  );
  await dialog.getByTestId('channel-submit-button').click();
  const response = await creation;
  expect(response.ok()).toBe(true);
  expect((await response.json()).errors).toBeUndefined();
  await expect(dialog).not.toBeVisible();
}

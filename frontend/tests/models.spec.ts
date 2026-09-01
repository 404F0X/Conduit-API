import { test, expect, type Page } from '@playwright/test'
import { gotoAndEnsureAuth, waitForGraphQLOperation } from './auth.utils'

const mockUpstreamURL = process.env.CONDUIT_E2E_MOCK_UPSTREAM_URL
if (!mockUpstreamURL) {
  throw new Error('CONDUIT_E2E_MOCK_UPSTREAM_URL is required; run tests through test:e2e')
}

async function performGraphQLOperation(page: Page, operationName: string, action: () => Promise<void>) {
  const responsePromise = page.waitForResponse(
    (response) => {
      if (!response.url().includes('/admin/graphql')) return false
      return response.request().postData()?.includes(operationName) ?? false
    },
    { timeout: 15000 }
  )

  await action()
  const response = await responsePromise
  expect(response.ok()).toBe(true)
  const payload = await response.json()
  expect(payload.errors, JSON.stringify(payload.errors)).toBeUndefined()
}

async function completeModelSettingsOnboarding(page: Page) {
  const driverPopover = page.locator('#driver-popover-content')
  const onboardingAppeared = await driverPopover
    .waitFor({ state: 'visible', timeout: 3000 })
    .then(() => true)
    .catch(() => false)
  if (!onboardingAppeared) return

  const driverOverlay = page.locator('.driver-overlay')
  const settingsButton = page.locator('[data-settings-button].driver-active-element')
  const settingsDialog = page
    .locator('[data-slot="dialog-content"]')
    .filter({ hasText: /Model Settings|模型设置/i })

  await expect(settingsButton).toBeVisible()
  const [response] = await Promise.all([
    page.waitForResponse(
      (candidate) => {
        if (!candidate.url().includes('/admin/graphql')) return false
        return candidate.request().postData()?.includes('CompleteSystemModelSettingOnboarding') ?? false
      },
      { timeout: 15000 }
    ),
    settingsButton.click(),
  ])

  await expect(driverOverlay).not.toBeVisible({ timeout: 5000 })
  expect(response.ok()).toBe(true)
  const payload = await response.json()
  expect(payload.errors, JSON.stringify(payload.errors)).toBeUndefined()

  // Completing the tour intentionally opens model settings. Close it before
  // the test starts interacting with catalog controls.
  if (await settingsDialog.isVisible().catch(() => false)) {
    await settingsDialog.locator('[data-slot="dialog-close"]').click()
    await expect(settingsDialog).not.toBeVisible({ timeout: 5000 })
  }
}

async function openModelsManagement(page: Page) {
  const publicModelsView = page.getByTestId('models-catalog-view-models')
  await publicModelsView.waitFor({ state: 'visible', timeout: 20000 })
  await publicModelsView.click()

  const enterpriseToggle = page.getByTestId('models-enterprise-toggle')
  await enterpriseToggle.waitFor({ state: 'visible', timeout: 20000 })
  if ((await enterpriseToggle.getAttribute('aria-expanded')) !== 'true') {
    await enterpriseToggle.click()
  }
  await expect(page.getByTestId('models-enterprise-panel')).toBeVisible()
  await expect(page.getByTestId('models-table')).toBeVisible({ timeout: 20000 })
}

async function createRouteChannel(page: Page, uniqueSuffix: string) {
  const channelName = `pw-model-route-${uniqueSuffix}`
  await gotoAndEnsureAuth(page, '/channels')

  const channelsTable = page.getByTestId('channels-table')
  await expect(channelsTable).toBeVisible({ timeout: 20000 })
  await page.getByTestId('add-channel-button').click()

  const createDialog = page.getByRole('dialog').filter({ has: page.getByTestId('channel-submit-button') })
  await expect(createDialog).toBeVisible()
  await createDialog.getByTestId('channel-name-input').fill(channelName)
  await createDialog.locator('#channel-billing-currency').fill('CNY')
  await createDialog.locator('#channel-recharge-multiplier').fill('1')
  await createDialog.getByTestId('provider-openai').click()
  await createDialog.getByTestId('channel-base-url-input').fill(mockUpstreamURL)
  await createDialog.getByTestId('channel-api-key-input').fill(`sk-model-route-${uniqueSuffix}`)

  await createDialog.getByTestId('quick-model-gpt-4o').click()
  const addSelected = createDialog.getByTestId('add-selected-models-button')
  await expect(addSelected).toBeEnabled()
  await addSelected.click()

  const defaultModel = createDialog.getByTestId('default-test-model-select')
  await expect(defaultModel).toBeEnabled()
  await defaultModel.click()
  await page.getByRole('option', { name: 'gpt-4o', exact: true }).click()

  await performGraphQLOperation(page, 'CreateChannel', () => createDialog.getByTestId('channel-submit-button').click())
  await expect(createDialog).not.toBeVisible({ timeout: 10000 })

  const channelRow = channelsTable.locator('tbody tr').filter({ hasText: channelName })
  await expect(channelRow).toBeVisible({ timeout: 10000 })
  const statusSwitch = channelRow.getByTestId('channel-status-switch')
  await expect(statusSwitch).not.toBeChecked()
  await statusSwitch.click()

  const statusDialog = page.getByRole('alertdialog').filter({ hasText: channelName })
  await expect(statusDialog).toBeVisible()
  await performGraphQLOperation(page, 'UpdateChannelStatus', () =>
    statusDialog.getByRole('button', { name: /Enable|启用/i, exact: true }).click()
  )
  await expect(statusDialog).not.toBeVisible({ timeout: 10000 })
  await expect(statusSwitch).toBeChecked()

  return channelName
}

async function deleteRouteChannel(page: Page, channelName: string) {
  await gotoAndEnsureAuth(page, '/channels')
  const channelsTable = page.getByTestId('channels-table')
  await expect(channelsTable).toBeVisible({ timeout: 20000 })
  const channelRow = channelsTable.locator('tbody tr').filter({ hasText: channelName })
  await expect(channelRow).toBeVisible({ timeout: 10000 })
  await channelRow.getByTestId('row-actions').click()
  await page.getByRole('menuitem', { name: /Delete|删除/i, exact: true }).click()

  const deleteDialog = page.getByRole('alertdialog').filter({ hasText: channelName })
  await expect(deleteDialog).toBeVisible()
  await deleteDialog.getByLabel(/Please enter Channel name|请输入渠道名称/i).fill(channelName)
  await performGraphQLOperation(page, 'DeleteChannel', () =>
    deleteDialog.getByRole('button', { name: /Delete Channel|删除渠道/i, exact: true }).click()
  )
  await expect(deleteDialog).not.toBeVisible({ timeout: 10000 })
  await expect(channelRow).toHaveCount(0)
}

test.describe('Admin Models Management', () => {
  test.beforeEach(async ({ page }) => {
    test.setTimeout(60000)
    await gotoAndEnsureAuth(page, '/models')

    await completeModelSettingsOnboarding(page)
    await openModelsManagement(page)
  })

  test('can create, edit, filter, toggle status, and delete a model', async ({ page }) => {
    test.setTimeout(120000)
    const uniqueSuffix = Date.now().toString().slice(-6)
    const baseName = `pw-model-${uniqueSuffix}`
    const updatedName = `${baseName}-updated`

    // Create one real, loopback-only channel. The production channel inventory
    // trigger materializes its supported model as an upstream deployment.
    const channelName = await createRouteChannel(page, uniqueSuffix)
    await gotoAndEnsureAuth(page, '/models')
    await openModelsManagement(page)

    // Open create dialog
    const createButton = page
      .getByRole('button', { name: /Add Public Model|添加对外模型/i, exact: true })
      .first()
    await expect(createButton).toBeVisible()
    await createButton.click()

    const dialog = page
      .getByRole('dialog')
      .filter({ has: page.getByRole('button', { name: /Create model and routes|创建模型与路由/i }) })
    await expect(dialog).toBeVisible()

    const upstreamSearch = dialog.getByPlaceholder(/Search upstream model ID|搜索上游模型 ID/i)
    await upstreamSearch.fill(channelName)
    const routeButton = dialog.getByRole('button', {
      name: new RegExp(`${channelName},\\s*gpt-4o`, 'i'),
    })
    await expect(routeButton).toBeVisible()
    await routeButton.click()
    await expect(routeButton).toHaveAttribute('aria-pressed', 'true')

    // The create flow uses labelled native inputs for the public ID and
    // developer. Scope by accessible name so the adjacent Radix type Select is
    // never mistaken for an editable combobox.
    await dialog.getByRole('textbox', { name: /Public model ID|对外模型 ID/i }).fill(baseName)
    await dialog.getByRole('textbox', { name: /Name|名称/i, exact: true }).fill(baseName)
    await dialog.getByRole('combobox', { name: /Developer|开发者/i }).fill('openai')

    await performGraphQLOperation(page, 'CreatePublicModelWithRoutes', () =>
      dialog.getByRole('button', { name: /Create model and routes|创建模型与路由/i, exact: true }).click()
    )
    await expect(dialog).not.toBeVisible({ timeout: 20000 })
    await waitForGraphQLOperation(page, 'GetModels')

    const modelsTable = page.getByTestId('models-table')
    const createdRow = modelsTable.locator('tbody tr').filter({ hasText: baseName })
    await expect(createdRow).toBeVisible({ timeout: 20000 })

    // Edit the created model
    const rowActions = createdRow.getByTestId('row-actions').first()
    await rowActions.click()
    const editMenuItem = page.getByRole('menuitem', { name: /Edit|编辑/i }).first()
    await editMenuItem.click()

    const editDialog = page.getByRole('dialog').filter({ hasText: /Edit Model|编辑/i }).first()
    await expect(editDialog).toBeVisible()
    const editNameInput = editDialog.getByLabel(/Name|名称/i)
    await editNameInput.fill(updatedName)

    await Promise.all([
      waitForGraphQLOperation(page, 'UpdateModel'),
      editDialog.getByRole('button', { name: /Save|保存|Update|更新/i }).last().click(),
    ])
    await expect(editDialog).not.toBeVisible({ timeout: 20000 })
    await waitForGraphQLOperation(page, 'GetModels')

    const updatedRow = modelsTable.locator('tbody tr').filter({ hasText: updatedName })
    await expect(updatedRow).toBeVisible({ timeout: 20000 })

    // Verify filtering by name works with the updated name
    const filterInput = page.getByPlaceholder(/Filter by name|名称|搜索/i)
    await filterInput.fill(updatedName)
    await expect(updatedRow).toBeVisible()
    await filterInput.fill('')

    // Toggle status via switch (enable/disable)
    const statusSwitch = updatedRow.locator('[data-testid="model-status-switch"]').first()
    await statusSwitch.click()
    const statusDialog = page.getByRole('alertdialog').or(page.getByRole('dialog'))
    await expect(statusDialog).toBeVisible()
    const confirmStatusButton = statusDialog
      .getByRole('button', { name: /Confirm|确认|确定|Enable|Disable/i })
      .last()
    await Promise.all([
      waitForGraphQLOperation(page, 'UpdateModel'),
      confirmStatusButton.click(),
    ])
    await expect(statusDialog).not.toBeVisible({ timeout: 20000 })
    await waitForGraphQLOperation(page, 'GetModels')

    // Delete the created model
    const actionsAfterToggle = updatedRow.getByTestId('row-actions').first()
    await actionsAfterToggle.click()
    const deleteMenuItem = page.getByRole('menuitem', { name: /Delete|删除/i }).first()
    await deleteMenuItem.click()

    const deleteDialog = page.getByRole('alertdialog').or(page.getByRole('dialog'))
    await expect(deleteDialog).toBeVisible()
    const deleteButton = deleteDialog.getByRole('button', { name: /Delete|删除|Confirm|确认/i }).last()
    await Promise.all([
      waitForGraphQLOperation(page, 'DeleteModel'),
      deleteButton.click(),
    ])
    await expect(deleteDialog).not.toBeVisible({ timeout: 20000 })
    await waitForGraphQLOperation(page, 'GetModels')

    await expect(modelsTable.locator('tbody tr').filter({ hasText: updatedName })).toHaveCount(0)
    await deleteRouteChannel(page, channelName)
  })

  test('opens an upstream deployment detail from a channel model link when fixtures provide one', async ({ page }) => {
    await gotoAndEnsureAuth(page, '/models?view=channels')

    const channelCard = page.getByTestId('channel-catalog-card').first()
    if (!(await channelCard.isVisible({ timeout: 10000 }).catch(() => false))) {
      test.skip(true, 'No channel fixture is available for upstream deployment coverage')
    }

    const upstreamModelLink = channelCard.getByTestId('upstream-model-link').first()
    if (!(await upstreamModelLink.isVisible({ timeout: 10000 }).catch(() => false))) {
      test.skip(true, 'The available channel fixture has no discovered upstream deployment')
    }

    await expect(channelCard.locator('a a')).toHaveCount(0)
    const upstreamHref = await upstreamModelLink.getAttribute('href')
    expect(upstreamHref).toContain('channel=')
    expect(upstreamHref).toContain('upstreamModel=')
    await upstreamModelLink.click()
    const detail = page.getByTestId('upstream-model-detail')
    await expect(detail).toBeVisible()
    await expect(detail.getByText(/Exact upstream-model health|精确上游模型健康度/i)).toBeVisible()
    const purchasePrice = detail.locator('[aria-labelledby="upstream-purchase-price"]')
    const suppliedModels = detail.locator('[aria-labelledby="upstream-public-models"]')
    await expect(purchasePrice).toBeVisible()
    await expect(purchasePrice).toContainText(/CNY\s+\d/i)
    await expect(purchasePrice).not.toContainText(/No purchase price is configured|尚未配置进货价格/i)
    await expect(suppliedModels).toBeVisible()
    await expect(suppliedModels).not.toContainText(/not connected to a public model|未连接任何对外模型/i)
    await expect(suppliedModels.locator('tbody tr a').first()).toBeVisible()
    await expect(detail.getByTestId('upstream-model-health-loading')).not.toBeVisible({ timeout: 20000 })
    await expect(page).toHaveURL(/view=channels.*channel=.*upstreamModel=/)
  })
})

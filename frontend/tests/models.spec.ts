import { test, expect } from '@playwright/test'
import { gotoAndEnsureAuth, waitForGraphQLOperation } from './auth.utils'

test.describe('Admin Models Management', () => {
  test.beforeEach(async ({ page }) => {
    test.setTimeout(60000)
    await gotoAndEnsureAuth(page, '/models')

    // The public-model catalog is the default product view. Enterprise model
    // entities and association tools intentionally live behind this disclosure.
    const publicModelsView = page.getByTestId('models-catalog-view-models')
    await publicModelsView.waitFor({ state: 'visible', timeout: 20000 })
    await publicModelsView.click()

    const enterpriseToggle = page.getByTestId('models-enterprise-toggle')
    await enterpriseToggle.waitFor({ state: 'visible', timeout: 20000 })
    if ((await enterpriseToggle.getAttribute('aria-expanded')) !== 'true') {
      await enterpriseToggle.click()
    }
    await expect(page.getByTestId('models-enterprise-panel')).toBeVisible()

    const modelsTable = page.getByTestId('models-table')
    await modelsTable.waitFor({ state: 'visible', timeout: 20000 })

    // The one-step tour is intentionally completed by clicking its highlighted
    // settings button; wait for its delayed mount before opening other dialogs.
    await page.waitForTimeout(700)
    const driverOverlay = page.locator('.driver-overlay')
    if (await driverOverlay.isVisible().catch(() => false)) {
      const settingsButton = page.locator('[data-settings-button]')
      if (await settingsButton.isVisible().catch(() => false)) {
        await Promise.all([
          waitForGraphQLOperation(page, 'CompleteSystemModelSettingOnboarding'),
          settingsButton.click(),
        ])
      }
      await expect(driverOverlay).not.toBeVisible({ timeout: 10000 })
    }

    // Close any dialog that may have opened (e.g., settings dialog from clicking the button)
    const settingsDialog = page.getByRole('dialog').filter({ hasText: /Model Settings|模型设置/i })
    if (await settingsDialog.isVisible().catch(() => false)) {
      await page.keyboard.press('Escape')
      await expect(settingsDialog).not.toBeVisible({ timeout: 5000 })
    }
  })

  test('can create, edit, filter, toggle status, and delete a model', async ({ page }) => {
    const uniqueSuffix = Date.now().toString().slice(-6)
    const baseName = `pw-model-${uniqueSuffix}`
    const updatedName = `${baseName}-updated`

    // Open create dialog
    const createButton = page
      .getByRole('button', { name: /Add Model|创建模型|新增模型/i, exact: true })
      .first()
    await expect(createButton).toBeVisible()
    await createButton.click()

    const dialog = page.locator('[data-slot="dialog-content"]')
    await expect(dialog).toBeVisible()

    // Select developer
    const developerCombo = dialog.locator('[role="combobox"]').first()
    await developerCombo.fill('moonshot')
    await developerCombo.press('Enter')

    // Select a modelId from provider list
    const modelIdInput = dialog.getByPlaceholder(/model id/i).first()
    await modelIdInput.click()
    await modelIdInput.fill('kimi')
    const modelOption = page.getByRole('option', { name: /kimi-k2-thinking/i }).first()
    await expect(modelOption).toBeVisible()
    await modelOption.click()

    // Override default name/group with deterministic values
    const nameInput = dialog.getByLabel(/Name|名称/i)
    await nameInput.fill(baseName)
    const groupInput = dialog.getByLabel(/Group|分组/i)
    await groupInput.fill(`group-${uniqueSuffix}`)
    const remarkInput = dialog.getByLabel(/Remark|备注/i)
    if (await remarkInput.count()) {
      await remarkInput.fill('Created via Playwright E2E')
    }

    const createResponsePromise = page.waitForResponse((response) => {
      if (!response.url().includes('/admin/graphql')) return false
      return response.request().postData()?.includes('CreateModel') ?? false
    })
    await dialog.getByRole('button', { name: /Create|创建|保存|Save/i }).last().click()
    const createResponse = await createResponsePromise
    const createPayload = await createResponse.json()
    expect(createPayload.errors, JSON.stringify(createPayload.errors)).toBeUndefined()
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
    await page.waitForTimeout(800)
    await expect(updatedRow).toBeVisible()
    await filterInput.fill('')
    await page.waitForTimeout(400)

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

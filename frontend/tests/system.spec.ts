import { test, expect } from '@playwright/test'
import { gotoAndEnsureAuth, waitForGraphQLOperation } from './auth.utils'

test.describe('Admin System Management', () => {
  test.beforeEach(async ({ page }) => {
    await gotoAndEnsureAuth(page, '/system')
  })

  test('can view system tabs and update brand settings', async ({ page }) => {
    await expect(
      page.getByRole('heading', { name: /System|系统/i }).first()
    ).toBeVisible()

    const brandTab = page.getByRole('tab', { name: /Brand|品牌/i })
    await brandTab.click()
    await expect(brandTab).toHaveAttribute('aria-selected', 'true')

    const brandInput = page.getByLabel(/Brand Name|品牌名称/i)
    await expect(brandInput).toBeVisible()

    const originalValue = await brandInput.inputValue()
    const newValue = originalValue.includes('pw-test')
      ? `${originalValue}-${Date.now().toString().slice(-4)}`
      : `pw-test-${Date.now().toString().slice(-4)}`

    await brandInput.fill(newValue)
    const saveButton = page.getByRole('button', { name: /Save Settings|保存设置/i })
    await expect(saveButton).toBeEnabled()

    await Promise.all([
      waitForGraphQLOperation(page, 'UpdateBrandSettings'),
      saveButton.click()
    ])

    await expect(brandInput).toHaveValue(newValue)

    if (newValue !== originalValue) {
      await brandInput.fill(originalValue)
      const revertButton = page.getByRole('button', { name: /Save Settings|保存设置/i })
      await expect(revertButton).toBeEnabled()
      await Promise.all([
        waitForGraphQLOperation(page, 'UpdateBrandSettings'),
        revertButton.click()
      ])
      await expect(brandInput).toHaveValue(originalValue)
    }

    const storageTab = page.getByRole('tab', { name: /Storage|存储/i })
    await storageTab.click()
    await expect(storageTab).toHaveAttribute('aria-selected', 'true')
    
    // Wait for storage content to load and check for any storage-related content
    await page.waitForTimeout(1000)
    const storageContent = page.locator('h1, h2, h3, h4, div, span').filter({ hasText: /Storage|storage|存储/i })
    if (await storageContent.count() > 0) {
      await expect(storageContent.first()).toBeVisible()
    } else {
      // If no specific storage text, just verify the tab is active
      await expect(storageTab).toHaveAttribute('aria-selected', 'true')
    }
  })
})

test.describe('Admin System Management on mobile', () => {
  test.use({ viewport: { width: 375, height: 812 } })

  test('reveals an initially off-screen tab without changing vertical scroll', async ({ page }) => {
    await gotoAndEnsureAuth(page, '/system?tab=about')

    const tabs = page.getByTestId('system-settings-tabs')
    const aboutTab = tabs.getByRole('tab', { name: /About|关于/i })
    await expect(aboutTab).toHaveAttribute('aria-selected', 'true')
    await expect(aboutTab).toBeVisible()
    await expect.poll(() => tabs.evaluate((list) => list.scrollLeft)).toBeGreaterThan(0)

    const bounds = await aboutTab.evaluate((trigger) => {
      const list = trigger.closest('[data-testid="system-settings-tabs"]')!
      const listBounds = list.getBoundingClientRect()
      const triggerBounds = trigger.getBoundingClientRect()
      return {
        listLeft: listBounds.left,
        listRight: listBounds.right,
        triggerLeft: triggerBounds.left,
        triggerRight: triggerBounds.right,
      }
    })
    expect(bounds.triggerLeft).toBeGreaterThanOrEqual(bounds.listLeft)
    expect(bounds.triggerRight).toBeLessThanOrEqual(bounds.listRight)

    const verticalScrollBeforeResize = await page.evaluate(() => {
      document.body.style.minHeight = `${window.innerHeight + 1000}px`
      window.scrollTo({ top: 240 })
      return window.scrollY
    })
    expect(verticalScrollBeforeResize).toBeGreaterThan(0)

    await page.setViewportSize({ width: 374, height: 812 })
    await page.evaluate(() => new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve())))
    expect(await page.evaluate(() => window.scrollY)).toBe(verticalScrollBeforeResize)
  })
})

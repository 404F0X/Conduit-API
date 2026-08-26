import { expect, test } from '@playwright/test'
import { gotoAndEnsureAuth } from './auth.utils'

test.describe('Project Requests Management', () => {
  test.beforeEach(async ({ page }) => {
    await gotoAndEnsureAuth(page, '/project/requests')
    await page.getByTestId('requests-table').waitFor({ state: 'visible', timeout: 20000 })
  })

  test('shows the request activity page and table', async ({ page }) => {
    await expect(page.getByRole('heading', { name: /Request Logs|请求日志/i })).toBeVisible()
    await expect(page.getByTestId('requests-table')).toBeVisible()
  })

  test('provides request filters and refresh controls', async ({ page }) => {
    await expect(page.getByPlaceholder(/model/i)).toBeVisible()
    await expect(page.getByRole('button', { name: /Refresh|刷新/i, exact: true })).toBeVisible()
    await expect(page.getByRole('switch', { name: /Auto Refresh|自动刷新/i })).toBeVisible()
  })

  test('is reachable from project navigation', async ({ page }) => {
    const requestsLink = page.getByRole('link', { name: /Requests|请求/i, exact: true })
    await expect(requestsLink).toHaveAttribute('href', '/project/requests')
  })
})

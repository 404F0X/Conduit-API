import { test, expect } from '@playwright/test'
import { gotoAndEnsureAuth } from './auth.utils'

const mockOrigin = process.env.CONDUIT_E2E_MOCK_ORIGIN
if (!mockOrigin) {
  throw new Error('CONDUIT_E2E_MOCK_ORIGIN is required; run tests through test:e2e')
}
const mockVerificationURL = `${mockOrigin}/device`

test.describe('GitHub Copilot Device Flow', () => {
  test.beforeEach(async ({ page }) => {
    test.setTimeout(60000)
    await gotoAndEnsureAuth(page, '/channels')
    await page.waitForTimeout(2000)
    const channelsTable = page.locator('[data-testid="channels-table"]')
    await channelsTable.waitFor({ state: 'visible', timeout: 15000 })
  })

  test('device flow shows initial state with start button', async ({ page }) => {
    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await expect(createButton).toBeVisible({ timeout: 10000 })
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    await expect(createDialog).toBeVisible()

    // Select GitHub Copilot provider
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Verify device flow component shows start button
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await expect(startButton).toBeVisible()
    await expect(startButton).toBeEnabled()
  })

  test('device flow shows loading state when starting', async ({ page }) => {
    // Intercept the OAuth start API call
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      // Delay response to test loading state
      await new Promise((resolve) => setTimeout(resolve, 1000))
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'ABCD-EFGH',
          verification_uri: mockVerificationURL,
          expires_in: 900,
          interval: 5,
          session_id: 'test-session-id',
        }),
      })
    })
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ status: 'pending' }),
      })
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Click start button
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Verify loading state
    const loadingText = createDialog.getByText(/Starting.../i)
    await expect(loadingText).toBeVisible()

    // Wait for user code to appear
    await expect(createDialog.getByText('ABCD-EFGH')).toBeVisible({ timeout: 5000 })
  })

  test('device flow displays user code and verification URL', async ({ page }) => {
    // Mock OAuth start response
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'WXYZ-1234',
          verification_uri: mockVerificationURL,
          expires_in: 900,
          interval: 5,
          session_id: 'test-session-id',
        }),
      })
    })
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ status: 'pending' }),
      })
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Verify user code is displayed
    const userCode = createDialog.getByText('WXYZ-1234')
    await expect(userCode).toBeVisible({ timeout: 5000 })

    // Verify verification URL is displayed
    const verificationUrl = createDialog.getByText(mockVerificationURL)
    await expect(verificationUrl).toBeVisible()

    // Verify "Open GitHub" button exists
    const openGitHubButton = createDialog.getByRole('button', { name: 'Open GitHub', exact: true })
    await expect(openGitHubButton).toBeVisible()

    // Verify instructions are shown
    const instructions = createDialog.getByText(/Waiting for you to authorize/i)
    await expect(instructions).toBeVisible()
  })

  test('device flow handles error state', async ({ page }) => {
    // Mock OAuth start to fail
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 500,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'Failed to start device flow' }),
      })
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Verify error state is shown
    const errorMessage = createDialog.getByText('Failed to start device flow', { exact: true })
    await expect(errorMessage).toBeVisible({ timeout: 5000 })

    // Verify retry button is available
    const retryButton = createDialog.getByRole('button', { name: 'Retry', exact: true })
    await expect(retryButton).toBeVisible()
    await expect(retryButton).toBeEnabled()
  })

  test('device flow can be reset', async ({ page }) => {
    // Mock OAuth start response
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'RESET-TEST',
          verification_uri: mockVerificationURL,
          expires_in: 900,
          interval: 5,
          session_id: 'test-session-id',
        }),
      })
    })
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ status: 'pending' }),
      })
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Wait for user code to appear
    await expect(createDialog.getByText('RESET-TEST')).toBeVisible({ timeout: 5000 })

    // Click retry/reset button
    const retryButton = createDialog.getByRole('button', { name: 'Retry', exact: true })
    await retryButton.click()

    // Verify we're back to initial state (start button visible)
    await expect(createDialog.getByText(/Connect to GitHub Copilot/i)).toBeVisible()
  })

  test('device flow keeps OAuth credentials out of raw API key inputs', async ({ page }) => {
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    await expect(createDialog.getByTestId('channel-api-key-input')).toHaveCount(0)
    await expect(createDialog.getByText(/Connect to GitHub Copilot/i)).toBeVisible()
  })

  test('device flow polling handles success response', async ({ page }) => {
    let pollCount = 0

    // Mock OAuth start
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'POLL-TEST',
          verification_uri: mockVerificationURL,
          expires_in: 900,
          interval: 1, // Short interval for testing
          session_id: 'test-session-id',
        }),
      })
    })

    // Mock OAuth polling - return success on second poll
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      pollCount++
      if (pollCount >= 2) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            access_token: 'test-access-token',
            token_type: 'Bearer',
            expires_in: 3600,
            refresh_token: 'test-refresh-token',
            scope: 'read:user',
          }),
        })
      } else {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ status: 'pending' }),
        })
      }
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Wait for user code
    await expect(createDialog.getByText('POLL-TEST')).toBeVisible({ timeout: 5000 })

    // Wait for success state (credentials imported)
    await expect(createDialog.getByText('Connected successfully!', { exact: true })).toBeVisible({
      timeout: 10000,
    })

    expect(pollCount).toBeGreaterThanOrEqual(2)
  })

  test('device flow polling handles slow_down response', async ({ page }) => {
    let pollCount = 0

    // Mock OAuth start
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'SLOW-TEST',
          verification_uri: mockVerificationURL,
          expires_in: 900,
          interval: 1,
          session_id: 'test-session-id',
        }),
      })
    })

    // Mock OAuth polling - return slow_down then pending
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      pollCount++
      if (pollCount === 1) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ status: 'slow_down' }),
        })
      } else {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ status: 'pending' }),
        })
      }
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Wait for user code
    await expect(createDialog.getByText('SLOW-TEST')).toBeVisible({ timeout: 5000 })

    // Wait a bit to ensure polling continues
    await page.waitForTimeout(3000)

    // Verify we're still in waiting state
    await expect(createDialog.getByText(/Waiting for you to authorize/i)).toBeVisible()
  })

  test('device flow handles expired session', async ({ page }) => {
    // Mock OAuth start
    await page.route('**/admin/copilot/oauth/start**', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          device_code: 'test-device-code',
          user_code: 'EXP-TEST',
          verification_uri: mockVerificationURL,
          expires_in: 2, // Very short expiration
          interval: 1,
          session_id: 'test-session-id',
        }),
      })
    })

    // Mock OAuth polling - return expired
    await page.route('**/admin/copilot/oauth/poll**', async (route) => {
      await route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: { message: 'Device flow expired' },
        }),
      })
    })

    // Open create channel dialog
    const createButton = page.getByTestId('add-channel-button')
    await createButton.click()

    const createDialog = page.getByRole('dialog')
    const copilotProviderRadio = createDialog.getByTestId('provider-github_copilot')
    await copilotProviderRadio.click()

    // Start device flow
    const startButton = createDialog.getByText(/Connect to GitHub Copilot/i)
    await startButton.click()

    // Wait for error state
    await expect(createDialog.getByText('Device flow expired', { exact: true })).toBeVisible({
      timeout: 10000,
    })

    // Verify retry button is available
    const retryButton = createDialog.getByRole('button', { name: 'Retry', exact: true })
    await expect(retryButton).toBeVisible()
  })
})

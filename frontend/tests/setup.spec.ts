import { test, expect } from '@playwright/test'
import { signInAsAdmin } from './auth.utils'

// Type declaration for process
declare const process: {
  env: Record<string, string | undefined>
}

/**
 * Setup test that runs first to initialize the system
 * This test creates the initial owner account with random credentials
 */
test.describe.configure({ mode: 'serial' })

test.describe('System Setup', () => {
  test('initialize system with owner account', async ({ page }) => {
    // Listen for console messages and errors
    page.on('console', (msg) => {
      if (msg.type() === 'error') {
        console.log(`Browser console error: ${msg.text()}`)
      }
    })

    // Listen for page errors
    page.on('pageerror', (error) => {
      console.log(`Page error: ${error.message}`)
    })

    // Listen for failed requests
    page.on('requestfailed', (request) => {
      console.log(`Request failed: ${request.url()} - ${request.failure()?.errorText}`)
    })

    // Generate random owner credentials
    const ownerEmail = process.env.CONDUIT_ADMIN_EMAIL || 'my@example.com'
    const ownerPassword = process.env.CONDUIT_ADMIN_PASSWORD || 'pwd123456'

    // Store credentials in environment for other tests to use
    process.env.CONDUIT_ADMIN_EMAIL = ownerEmail

    console.log(`Initializing system with owner: ${ownerEmail}`)

    // Navigate to the app - should redirect to initialization if system is not initialized
    await page.goto('/', { waitUntil: 'domcontentloaded' })
    await page.waitForTimeout(2000)

    // Check if we're on initialization page or sign-in page
    const currentUrl = page.url()

    if (currentUrl.includes('/initialization')) {
      // System needs initialization
      console.log('System requires initialization')

      // Wait for initialization form
      await page.getByRole('textbox', { name: /Owner First Name/i }).waitFor({ timeout: 10000 })

      // Fill in all required fields
      await page.getByRole('textbox', { name: /Owner First Name/i }).fill('Admin')
      await page.getByRole('textbox', { name: /Owner Last Name/i }).fill('User')
      await page.getByRole('textbox', { name: /Owner Email/i }).fill(ownerEmail)
      await page.getByLabel(/^Owner Password$/i).fill(ownerPassword)
      const confirmPasswordField = page.getByLabel(/Confirm Owner Password/i)
      await confirmPasswordField.fill(`${ownerPassword}-typo`)
      await page.getByRole('textbox', { name: /Brand Name/i }).fill('Conduit API')

      // Submit initialization form
      const submitButton = page.getByRole('button', { name: /Create owner account|创建所有者账户/i })
      await expect(submitButton).toBeVisible()
      await expect(submitButton).toBeDisabled()
      await confirmPasswordField.fill(ownerPassword)
      await expect(submitButton).toBeEnabled()

      // Owner creation completes immediately; finance is confirmed after login.
      await Promise.all([
        page.waitForURL((url) => url.toString().includes('/sign-in'), {
          timeout: 15000,
        }),
        submitButton.click(),
      ])

      console.log('System initialized successfully, now on sign-in page')

      // Now sign in with the owner credentials using the utility function
      await signInAsAdmin(page, { email: ownerEmail, password: ownerPassword })
      console.log('Signed in successfully after initialization')
    } else if (currentUrl.includes('/sign-in')) {
      // System is already initialized, just sign in
      console.log('System already initialized, signing in')

      await signInAsAdmin(page, { email: ownerEmail, password: ownerPassword })
      console.log('Signed in successfully')
    } else {
      // Already logged in or on dashboard
      console.log('System already initialized and logged in')
    }

    // New installations require an authenticated, page-level financial setup.
    const financialHeading = page.getByRole('heading', {
      name: /Configure accounting currency and internal credits|配置记账货币与站内积分/i,
    })
    if (await financialHeading.isVisible({ timeout: 3000 }).catch(() => false)) {
      const currencyField = page.getByLabel(/ISO accounting currency|ISO 记账货币/i)
      await currencyField.click()
      await currencyField.fill('CNY')
      await page.getByRole('option', { name: /CNY/, exact: false }).click()
      await page.getByLabel(/Internal credit display name|站内积分显示名称/i).fill('Credits')
      await page.getByLabel(/Credits per accounting unit|每 1 单位记账货币对应积分数/i).fill('10000')
      const saveFinance = page.getByRole('button', { name: /Save financial foundation|保存财务基础/i })
      await expect(saveFinance).toBeEnabled()
      await saveFinance.click()
      await expect(financialHeading).toBeHidden({ timeout: 15000 })
    }

    // Verify we're logged in by checking we're not on sign-in or initialization page
    await expect(page.url()).not.toContain('/500')
    await expect(page.url()).not.toContain('/sign-in')
    await expect(page.url()).not.toContain('/initialization')

    // Handle onboarding dialog if it appears
    console.log('Checking for onboarding dialog...')
    try {
      // Wait for potential onboarding dialog to appear (with a short timeout)
      const skipButton = page.getByTestId('onboarding-skip-tour')
      await skipButton.waitFor({ timeout: 5000 })
      
      // Click the skip button to dismiss onboarding
      await skipButton.click()
      console.log('Onboarding dialog skipped successfully')
      
      // Wait a moment for the dialog to close
      await page.waitForTimeout(1000)
    } catch (error) {
      // No onboarding dialog appeared, which is fine
      console.log('No onboarding dialog found or already handled')
    }

    console.log(`Setup complete. Owner email: ${ownerEmail}`)
  })
})

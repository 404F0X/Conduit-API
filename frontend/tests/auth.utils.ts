import { expect, Page } from '@playwright/test'

// Type declaration for process
declare const process: {
  env: Record<string, string | undefined>
}

export interface AdminCredentials {
  email: string
  password: string
}

const defaultCredentials: AdminCredentials = {
  email: process.env.CONDUIT_ADMIN_EMAIL || 'my@example.com',
  password: process.env.CONDUIT_ADMIN_PASSWORD || 'pwd123456',
}

const ACCESS_TOKEN_KEY = 'conduit_access_token'

function signInEmailField(page: Page) {
  return page
    .getByTestId('sign-in-email')
    .or(page.locator('input[type="email"], input[name="email"]'))
    .first()
}

function signInPasswordField(page: Page) {
  return page
    .getByTestId('sign-in-password')
    .or(page.locator('input[type="password"], input[name="password"]'))
    .first()
}

function isSignInUrl(url: URL) {
  return url.pathname === '/sign-in' || url.pathname.endsWith('/sign-in')
}

function isRequestedUrl(url: URL, path: string) {
  const expected = new URL(path, 'http://127.0.0.1/')
  const pathnameMatches = url.pathname === expected.pathname || url.pathname.endsWith(expected.pathname)
  return pathnameMatches && url.search === expected.search
}

async function hasStoredAccessToken(page: Page) {
  try {
    return await page.evaluate(
      (key) => {
        const token = localStorage.getItem(key)
        return typeof token === 'string' && token.trim().length > 0
      },
      ACCESS_TOKEN_KEY
    )
  } catch {
    // A document navigation can briefly replace the execution context.
    return false
  }
}

async function waitForStoredAccessToken(page: Page) {
  await expect
    .poll(() => hasStoredAccessToken(page), {
      message: 'expected sign-in to persist a non-empty access token',
      timeout: 15000,
    })
    .toBe(true)
}

export async function signInAsAdmin(page: Page, credentials: AdminCredentials = defaultCredentials) {
  // Listen for console errors
  page.on('console', (msg) => {
    if (msg.type() === 'error') {
      console.log('Browser console error:', msg.text())
    }
  })

  // Listen for page errors
  page.on('pageerror', (error) => {
    console.log('Page error:', error.message)
  })

  // Wait for the page to fully load
  await page.waitForLoadState('domcontentloaded', { timeout: 15000 })

  // Wait for React to mount - check for root element content
  try {
    await page.waitForFunction(
      () => {
        const root = document.getElementById('root')
        return root && root.innerHTML.length > 100
      },
      { timeout: 15000 }
    )
  } catch (error) {
    console.log('Warning: Root element may not be fully loaded')
    console.log('Page URL:', page.url())

    // Check if root exists at all
    const rootExists = await page.evaluate(() => {
      const root = document.getElementById('root')
      return { exists: !!root, innerHTML: root?.innerHTML.substring(0, 200) }
    })
    console.log('Root element state:', rootExists)
  }

  // Wait for the login form to be visible using reliable test IDs
  // Fallback to multiple selectors for backward compatibility
  const emailField = signInEmailField(page)

  await emailField.waitFor({ state: 'visible', timeout: 20000 })

  // Fill in credentials with test IDs and fallback selectors
  const passwordField = signInPasswordField(page)

  await emailField.fill(credentials.email)
  await passwordField.fill(credentials.password)

  // Click login button - use test ID with fallback
  const loginButton = page.getByTestId('sign-in-submit').or(page.getByRole('button', { name: /登录|Sign In|Sign in/i }))
  await expect(loginButton).toBeVisible()

  // Wait for the sign-in API response before checking navigation
  const responsePromise = page.waitForResponse(
    (response) => response.url().includes('/admin/auth/signin') && response.request().method() === 'POST',
    { timeout: 15000 }
  )

  await loginButton.click()

  try {
    const response = await responsePromise
    expect(response.status(), 'expected the sign-in API to accept the credentials').toBe(200)
  } catch (error) {
    console.log(`Sign-in API error: ${error}`)
    // Take a screenshot for debugging
    const timestamp = Date.now()
    await page.screenshot({ path: `test-results/sign-in-error-${timestamp}.png`, fullPage: true })
    console.log('Page URL:', page.url())
    throw error
  }

  // The response event fires before React has necessarily consumed the body
  // and run the mutation success callback. Prove that callback completed
  // before a caller is allowed to start another document navigation.
  await waitForStoredAccessToken(page)
  await expect(emailField).toBeHidden({ timeout: 15000 })
  await page.waitForURL((url) => !isSignInUrl(url), { timeout: 15000 })
  await expect(page.locator('#content')).toBeVisible({ timeout: 20000 })

  // Verify the authenticated shell did not clear a rejected token while it
  // loaded the current user.
  await waitForStoredAccessToken(page)
}

export async function ensureSignedIn(page: Page) {
  if (page.url().includes('/sign-in')) {
    await signInAsAdmin(page)
  }

  // Verify we have a valid token
  const hasToken = await hasStoredAccessToken(page)

  if (!hasToken) {
    console.warn('Warning: No valid auth token found, attempting to sign in')
    await signInAsAdmin(page)
  }
}

export async function gotoAndEnsureAuth(page: Page, path: string) {
  // Navigate to the target path - let the app handle auth redirects naturally
  await page.goto(path, { waitUntil: 'domcontentloaded', timeout: 30000 })

  const emailField = signInEmailField(page)
  const authenticatedShell = page.locator('#content')

  // A protected route can briefly retain its URL while the auth guard renders
  // and redirects. Wait for a real UI state instead of guessing with a sleep.
  await expect(authenticatedShell.or(emailField)).toBeVisible({ timeout: 20000 })

  if (await emailField.isVisible()) {
    await signInAsAdmin(page)
  }

  // Login redirects to the product landing page. Only navigate again after
  // signInAsAdmin has proven token persistence and authenticated-shell load.
  if (!isRequestedUrl(new URL(page.url()), path)) {
    await page.goto(path, { waitUntil: 'domcontentloaded', timeout: 30000 })
  }

  await page.waitForURL((url) => isRequestedUrl(url, path), { timeout: 15000 })
  await waitForStoredAccessToken(page)
  await expect(authenticatedShell).toBeVisible({ timeout: 20000 })
  await expect(emailField).toBeHidden()
}

export async function waitForGraphQLOperation(page: Page, operationName: string) {
  const lowerCamel = operationName.length
    ? operationName.charAt(0).toLowerCase() + operationName.slice(1)
    : operationName
  try {
    await Promise.race([
      page.waitForResponse((response) => {
        const url = response.url()
        const isGraphQL = url.includes('/admin/graphql') || url.includes('/graphql')
        if (!isGraphQL) return false
        const body = response.request().postData()
        if (!body) return false
        return body.includes(operationName) || body.includes(lowerCamel)
      }),
      // Fallback to a short timeout to avoid hard failures when backend is unavailable
      page.waitForTimeout(4000),
    ])
  } catch {
    // Swallow errors to keep tests resilient in environments without backend
  }
}

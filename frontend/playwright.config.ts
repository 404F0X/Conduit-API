import { defineConfig, devices } from '@playwright/test'

const frontendPort = Number.parseInt(process.env.CONDUIT_E2E_FRONTEND_PORT ?? '9527', 10)
const frontendURL =
  process.env.CONDUIT_E2E_FRONTEND_URL ?? `http://127.0.0.1:${frontendPort}`
const backendURL = process.env.CONDUIT_API_URL ?? 'http://127.0.0.1:8099'
const mockOrigin = process.env.CONDUIT_E2E_MOCK_ORIGIN ?? 'http://127.0.0.1:18099'
if (!/^http:\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?::\d+)?\/?$/.test(mockOrigin)) {
  throw new Error('CONDUIT_E2E_MOCK_ORIGIN must be a loopback HTTP origin; use test:e2e')
}

process.env.CONDUIT_ADMIN_EMAIL ??= 'e2e-owner@conduit.invalid'
process.env.CONDUIT_ADMIN_PASSWORD ??= 'conduit-e2e-password-2026'
process.env.CONDUIT_API_URL = backendURL

/**
 * @see https://playwright.dev/docs/test-configuration
 */
export default defineConfig({
  testDir: './tests',
  /* The suite shares one isolated database and mutates global admin state. */
  fullyParallel: false,
  /* Fail the build on CI if you accidentally left test.only in the source code. */
  forbidOnly: !!process.env.CI,
  /* Retry on CI only */
  retries: process.env.CI ? 2 : 0,
  /* Keep mutations deterministic locally and in CI; opt in explicitly when
     individual tests gain per-worker database isolation. */
  workers: 1,
  /* Reporter to use. See https://playwright.dev/docs/test-reporters */
  reporter: 'html',
  /* Test match pattern - run setup.spec.ts first, then others */
  testMatch: ['**/*.spec.ts'],
  /* Shared settings for all the projects below. See https://playwright.dev/docs/api/class-testoptions. */
  use: {
    /* Base URL to use in actions like `await page.goto('/')`. */
    baseURL: frontendURL,

    /* Collect trace when retrying the failed test. See https://playwright.dev/docs/trace-viewer */
    trace: 'on-first-retry',

    /* Screenshot on failure */
    screenshot: 'only-on-failure',

    /* Video on failure */
    video: 'retain-on-failure',

    /* Route any accidental non-loopback browser traffic into the local mock. */
    proxy: { server: mockOrigin, bypass: '127.0.0.1,localhost,[::1]' },
  },

  /* Configure projects for major browsers */
  projects: [
    // Setup project - runs first to initialize the system
    {
      name: 'setup',
      testMatch: '**/setup.spec.ts',
      use: { ...devices['Desktop Chrome'] },
    },
    // Main test suite - runs after setup
    {
      name: 'chromium',
      testIgnore: '**/setup.spec.ts',
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'] },
    },

    // {
    //   name: 'firefox',
    //   use: { ...devices['Desktop Firefox'] },
    // },

    // {
    //   name: 'webkit',
    //   use: { ...devices['Desktop Safari'] },
    // },

    /* Test against mobile viewports. */
    // {
    //   name: 'Mobile Chrome',
    //   use: { ...devices['Pixel 5'] },
    // },
    // {
    //   name: 'Mobile Safari',
    //   use: { ...devices['iPhone 12'] },
    // },

    /* Test against branded browsers. */
    // {
    //   name: 'Microsoft Edge',
    //   use: { ...devices['Desktop Edge'], channel: 'msedge' },
    // },
    // {
    //   name: 'Google Chrome',
    //   use: { ...devices['Desktop Chrome'], channel: 'chrome' },
    // },
  ],

  /* Run your local dev server before starting the tests */
  webServer: {
    command: 'pnpm dev --host 127.0.0.1 --strictPort',
    url: frontendURL,
    reuseExistingServer: false,
    timeout: 120 * 1000, // 2 minutes timeout
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      VITE_PORT: String(frontendPort),
      VITE_API_URL: backendURL,
    },
  },
})

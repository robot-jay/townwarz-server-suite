import { defineConfig } from '@playwright/test';

const live = !!process.env.TEST_REAL_GAME_WS_URL;
const port = Number(process.env.TEST_BROWSER_PORT ?? (live ? 5177 : 5176));
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: './tests',
  testMatch: live ? '**/phase3-live.spec.ts' : '**/construction.spec.ts',
  outputDir: live ? 'test-results/live' : 'test-results/mocked',
  timeout: 45000,
  expect: { timeout: 10000 },
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: 'list',
  use: { baseURL, viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block', trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${port}`,
    url: baseURL,
    reuseExistingServer: false,
    timeout: 120000,
    env: { VITE_AUTH0_DOMAIN: 'fixture.auth0.test', VITE_AUTH0_CLIENT_ID: 'fixture-client', VITE_GAME_WS_URL: process.env.TEST_REAL_GAME_WS_URL ?? 'ws://127.0.0.1:8081/play' },
  },
});

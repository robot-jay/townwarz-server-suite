import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  timeout: 45000,
  expect: { timeout: 10000 },
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: 'list',
  use: { baseURL: 'http://127.0.0.1:5176', viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block', trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 5176',
    url: 'http://127.0.0.1:5176',
    reuseExistingServer: false,
    timeout: 120000,
    env: { VITE_AUTH0_DOMAIN: 'fixture.auth0.test', VITE_AUTH0_CLIENT_ID: 'fixture-client', VITE_GAME_WS_URL: 'ws://127.0.0.1:8081/play' },
  },
});

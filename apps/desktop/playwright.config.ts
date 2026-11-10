import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: 'test',
  timeout: 30_000,
  reporter: [['list']],
  use: {
    browserName: 'chromium',
    baseURL: 'http://localhost:1420',
    viewport: { width: 1000, height: 820 },
    colorScheme: 'dark',
  },
  webServer: {
    command: 'node scripts/build.mjs --serve',
    url: 'http://localhost:1420',
    reuseExistingServer: !process.env.CI,
  },
});

import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: 'test/ui',
  timeout: 30_000,
  reporter: [['list']],
  use: { browserName: 'chromium', viewport: { width: 1280, height: 860 } },
});

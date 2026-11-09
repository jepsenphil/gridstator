import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

const pkg = (p: string) => fileURLToPath(new URL(p, import.meta.url));

export const alias = {
  '@gridstator/shared': pkg('./packages/shared/src/index.ts'),
  '@gridstator/protocol': pkg('./packages/protocol/src/index.ts'),
  '@gridstator/scheduler': pkg('./packages/scheduler/src/index.ts'),
  '@gridstator/discovery': pkg('./packages/discovery/src/index.ts'),
  '@gridstator/reports': pkg('./packages/reports/src/index.ts'),
  '@gridstator/snapshot': pkg('./packages/snapshot/src/index.ts'),
  '@gridstator/executors': pkg('./packages/executors/src/index.ts'),
  '@gridstator/client': pkg('./packages/client/src/index.ts'),
  '@gridstator/controller': pkg('./apps/controller/src/index.ts'),
  '@gridstator/agent': pkg('./apps/agent/src/index.ts'),
};

export default defineConfig({
  resolve: { alias: { ...alias, vscode: pkg('./apps/vscode/src/test-vscode.ts') } },
  test: {
    include: ['packages/*/src/**/*.test.ts', 'apps/*/src/**/*.test.ts'],
    testTimeout: 20_000,
  },
});

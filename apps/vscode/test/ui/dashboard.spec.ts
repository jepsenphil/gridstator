import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import type { FromWebview, ToWebview } from '../../src/messages';
import { FAILED, GROUP, RUNNING, state } from './fixtures';

const media = join(__dirname, '../../media') + '/';
const shots = join(__dirname, 'screenshots') + '/';
mkdirSync(shots, { recursive: true });

// A dark theme similar to VS Code's default, expressed with the variables the dashboard uses.
const THEME = `
:root {
  --vscode-font-family: system-ui, sans-serif; --vscode-font-size: 13px; --vscode-editor-font-family: monospace; --vscode-editor-font-size: 12px;
  --vscode-foreground: #cccccc; --vscode-descriptionForeground: #9d9d9d; --vscode-editor-background: #1f1f1f;
  --vscode-editorWidget-background: #252526; --vscode-widget-border: #3c3c3c; --vscode-list-hoverBackground: #2a2d2e;
  --vscode-button-background: #0078d4; --vscode-button-foreground: #ffffff; --vscode-button-hoverBackground: #026ec1;
  --vscode-button-secondaryBackground: #313131; --vscode-button-secondaryForeground: #cccccc; --vscode-button-secondaryHoverBackground: #3c3c3c;
  --vscode-input-background: #313131; --vscode-input-foreground: #cccccc; --vscode-input-border: #3c3c3c; --vscode-focusBorder: #0078d4;
  --vscode-dropdown-background: #313131; --vscode-dropdown-foreground: #cccccc; --vscode-badge-background: #616161; --vscode-badge-foreground: #f8f8f8;
  --vscode-textLink-foreground: #4daafc; --vscode-charts-blue: #3794ff; --vscode-testing-iconPassed: #73c991; --vscode-testing-iconFailed: #f14c4c;
  --vscode-editorWarning-foreground: #cca700; --vscode-terminal-background: #181818; --vscode-list-activeSelectionBackground: #04395e; --vscode-list-activeSelectionForeground: #ffffff;
  --vscode-textCodeBlock-background: #2b2b2b; --vscode-inputValidation-errorBackground: #5a1d1d; --vscode-inputValidation-errorBorder: #be1100;
}`;

const LIGHT_THEME = `${THEME}\n:root {
  --vscode-foreground: #1f1f1f; --vscode-descriptionForeground: #575757; --vscode-editor-background: #ffffff;
  --vscode-editorWidget-background: #f6f6f6; --vscode-widget-border: #d4d4d4; --vscode-list-hoverBackground: #eeeeee;
  --vscode-button-secondaryBackground: #e5e5e5; --vscode-button-secondaryForeground: #262626; --vscode-button-secondaryHoverBackground: #d8d8d8;
  --vscode-input-background: #ffffff; --vscode-input-foreground: #1f1f1f; --vscode-input-border: #b6b6b6;
  --vscode-dropdown-background: #ffffff; --vscode-dropdown-foreground: #1f1f1f; --vscode-badge-background: #e5e5e5; --vscode-badge-foreground: #262626;
  --vscode-textLink-foreground: #005fb8; --vscode-testing-iconPassed: #237b4b; --vscode-testing-iconFailed: #b5200d;
  --vscode-editorWarning-foreground: #8a5d00; --vscode-textCodeBlock-background: #f0f0f0;
  --vscode-inputValidation-errorBackground: #fff0f0; --vscode-inputValidation-errorBorder: #b5200d;
  --vscode-list-activeSelectionBackground: #cce8ff; --vscode-list-activeSelectionForeground: #1f1f1f;
}`;

async function open(page: Page, initial = state(), theme = THEME): Promise<void> {
  const file = join(shots, '..', '.fixture.html');
  writeFileSync(
    file,
    `<!doctype html><html><head><meta charset="utf-8"><style>${theme}</style>
     <link rel="stylesheet" href="file://${media}codicons/codicon.css"><link rel="stylesheet" href="file://${media}dashboard.css"></head>
     <body><div id="root"></div>
     <script>window.__sent = []; window.acquireVsCodeApi = () => ({ postMessage: (m) => window.__sent.push(m), getState: () => undefined, setState: (state) => { window.__saved = state; } });</script>
     <script src="file://${media}dashboard.js"></script></body></html>`,
  );
  await page.goto(`file://${file}`);
  await expect.poll(() => sent(page)).toContainEqual({ type: 'ready' });
  await push(page, { type: 'state', state: initial });
}

async function push(page: Page, msg: ToWebview): Promise<void> {
  await page.evaluate((m) => window.postMessage(m, '*'), msg);
}

async function sent(page: Page): Promise<FromWebview[]> {
  return page.evaluate(() => (window as unknown as { __sent: FromWebview[] }).__sent);
}

test('overview summarises nodes and jobs', async ({ page }) => {
  await open(page);
  await expect(page.getByText('nodes online')).toBeVisible();
  await expect(page.locator('.tile').first()).toContainText('2/3');
  await expect(page.locator('.node-list')).toContainText('big-box');
  await expect(page.locator('.node-list')).toContainText('draining');
  await expect(page.getByText('waiting for 8 CPU, 16 GiB on node big-box')).toBeVisible();
  await page.screenshot({ path: `${shots}overview.png`, fullPage: true });
});

test('jobs list filters and its row actions post cancel and retry', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Jobs/ }).click();
  await page.getByRole('tab', { name: /Failed/ }).click();
  await expect(page.locator('.jobs-table tbody tr')).toHaveCount(1);
  await page.getByTitle('Retry (same snapshot)').click();
  await page.getByRole('tab', { name: /^All/ }).click();
  await expect(page.locator('.jobs-table tbody tr')).toHaveCount(4);
  await page.locator('tr', { hasText: 'actions ci.yml' }).getByTitle('Cancel').click();
  const msgs = await sent(page);
  expect(msgs).toContainEqual({ type: 'retry', jobId: FAILED });
  expect(msgs).toContainEqual({ type: 'cancel', jobId: RUNNING });
  await page.screenshot({ path: `${shots}jobs.png`, fullPage: true });
});

test('job details stream logs, filter them, open files and act on the job', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Jobs/ }).click();
  await page.locator('tr', { hasText: 'actions ci.yml' }).click();
  await expect.poll(() => sent(page)).toContainEqual({ type: 'selectJob', jobId: RUNNING });
  await push(page, {
    type: 'logs',
    jobId: RUNNING,
    reset: true,
    entries: [
      { s: 1, t: 1, o: 'system', d: 'Job accepted by node big-box\n' },
      {
        s: 2,
        t: 2,
        o: 'stdout',
        d: '[CI/test] ⭐ Run Main Run tests\n[CI/test]   | ✔ adds (1ms)\n',
      },
      {
        s: 3,
        t: 3,
        o: 'stderr',
        d: '[CI/test]   | AssertionError: expected 4 to equal 5\n[CI/test]   |     at /workspace/test/math.test.js:9:10\n',
      },
      { s: 4, t: 4, o: 'stdout', d: 'partial ' },
      { s: 5, t: 5, o: 'stdout', d: 'line joined\n' },
    ],
  });
  await expect(page.locator('.log-line')).toHaveCount(6);
  await expect(page.locator('.log-line', { hasText: 'partial line joined' })).toHaveCount(1);
  await expect(page.locator('.steps')).toContainText('Run tests');
  await page.screenshot({ path: `${shots}job-running.png`, fullPage: true });

  await page.getByPlaceholder('Filter log lines').fill('assertion');
  await expect(page.locator('.log-line')).toHaveCount(1);
  await page.getByPlaceholder('Filter log lines').fill('');
  await page.locator('.log-link').click();
  await page.getByRole('button', { name: 'Cancel' }).click();
  const msgs = await sent(page);
  expect(msgs).toContainEqual({
    type: 'openFile',
    jobId: RUNNING,
    path: '/workspace/test/math.test.js',
    line: 9,
    column: 10,
  });
  expect(msgs).toContainEqual({ type: 'cancel', jobId: RUNNING });
});

test('finished job offers retry, rerun and artifact download', async ({ page }) => {
  await open(page);
  await push(page, { type: 'navigate', view: 'job', jobId: FAILED });
  await expect(page.locator('.banner-error')).toContainText('Process exited with code 1');
  await page.getByRole('button', { name: 'Retry' }).click();
  await page.getByRole('button', { name: 'Rerun with current files' }).click();
  await page.getByTitle('Download reports/junit.xml.tar').click();
  const msgs = await sent(page);
  expect(msgs).toContainEqual({ type: 'retry', jobId: FAILED });
  expect(msgs).toContainEqual({ type: 'rerun', jobId: FAILED });
  expect(msgs).toContainEqual({
    type: 'downloadArtifact',
    jobId: FAILED,
    name: 'reports/junit.xml.tar',
  });
  await page.screenshot({ path: `${shots}job-failed.png`, fullPage: true });
});

test('run launcher builds workflow, Docker and command requests', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Run/ }).click();
  await page.locator('.run-item', { hasText: 'CI' }).click();
  await expect(page.locator('option', { hasText: 'cannot run locally' })).toHaveAttribute(
    'disabled',
    '',
  );
  await page.getByLabel('Job').selectOption('test');
  await page.getByLabel('Event').selectOption('workflow_dispatch');
  await page.getByLabel('greeting').fill('Gridstator');
  await page.getByLabel('Worker').selectOption('big-box');
  await page.screenshot({ path: `${shots}run-workflow.png`, fullPage: true });
  await page.getByRole('button', { name: 'Run job test' }).click();

  await page.locator('.run-item', { hasText: 'Dockerfile' }).click();
  await page.getByLabel('Target stage').selectOption('runner');
  await page.getByLabel('Tags').fill('web:dev, web:latest');
  await page.getByLabel('Output').selectOption('tar');
  await page.getByRole('button', { name: 'Build' }).click();

  await page.locator('.run-item', { hasText: 'Custom command' }).click();
  await page.getByLabel('Command').fill('npm ci && npm test');
  await page.getByRole('button', { name: 'Run', exact: true }).click();

  const launches = (await sent(page)).filter((m) => m.type === 'launch');
  expect(launches).toEqual([
    {
      type: 'launch',
      request: {
        itemId: 'workflow:ws:.github/workflows/ci.yml',
        options: {
          node: 'big-box',
          job: 'test',
          event: 'workflow_dispatch',
          inputs: { greeting: 'Gridstator' },
        },
      },
    },
    {
      type: 'launch',
      request: {
        itemId: 'dockerfile:ws:Dockerfile',
        options: { stage: 'runner', tags: ['web:dev', 'web:latest'], output: 'tar' },
      },
    },
    {
      type: 'launch',
      request: { itemId: 'command:ws', options: { command: 'npm ci && npm test' } },
    },
  ]);
});

test('curated workflows expose standalone jobs without a whole-workflow bypass', async ({
  page,
}) => {
  const entries = [
    [
      'staging-local.yml',
      '🧪 Staging: Local build / publish / deploy',
      'local',
      'Build staging locally; publish or deploy a saved build when ready.',
    ],
    ['coverage.yml', '✅ Checks: Coverage', 'coverage', 'Run the test suite with coverage.'],
    [
      'security.yml',
      '✅ Checks: Security scans',
      'dependency-and-secret-scan',
      'Scan dependencies and Git history for security findings.',
    ],
    [
      'database-integration.yml',
      '✅ Checks: Database integration',
      'database',
      'Run integration tests against isolated PostgreSQL and Redis.',
    ],
    [
      'playwright-e2e.yml',
      '✅ Checks: Playwright browser tests',
      'tests',
      'Run the general browser test suite.',
    ],
    [
      'onboarding-e2e.yml',
      '✅ Checks: Onboarding journeys',
      'journeys',
      'Run the full onboarding browser journeys.',
    ],
  ] as const;
  for (const [theme, css] of [
    ['dark', THEME],
    ['light', LIGHT_THEME],
  ] as const) {
    for (const width of [1280, 390]) {
      const initial = state();
      initial.launcher.items = entries.map(([file, label, id, description]) => ({
        id: `workflow:ws:.github/workflows/${file}`,
        kind: 'workflow',
        label,
        description,
        folder: 'ws',
        warnings: [],
        workflow: {
          file: `.github/workflows/${file}`,
          events: ['workflow_call'],
          defaultEvent: 'workflow_call',
          inputs: [],
          wholeWorkflow: false,
          jobs: [{ id, name: id, needs: [], runnable: true, warnings: [] }],
        },
      }));
      initial.jobs = [];
      initial.groups = [];
      const beforeCuration = structuredClone(initial);
      const previousWorkflow = beforeCuration.launcher.items[1]!.workflow!;
      delete previousWorkflow.wholeWorkflow;
      previousWorkflow.jobs.push({
        id: 'publish_badge',
        name: 'Publish badge',
        needs: [],
        runnable: true,
        warnings: [],
      });
      await page.setViewportSize({ width, height: 1000 });
      await open(page, beforeCuration, css);
      await page.getByRole('tab', { name: /Run/ }).click();
      await expect(page.locator('.run-item').filter({ hasText: /Internal/ })).toHaveCount(0);
      await page.locator('.run-item', { hasText: '✅ Checks: Coverage' }).click();
      await expect(page.getByLabel('Job')).toHaveValue('');
      await push(page, { type: 'state', state: initial });
      await expect(page.getByLabel('Job')).toHaveValue('coverage');
      await expect(
        page.getByRole('button', { name: 'Run job coverage', exact: true }),
      ).toBeEnabled();
      await expect(page.getByLabel('Job').locator('option')).toHaveCount(1);
      await expect(
        page.getByLabel('Job').getByRole('option', { name: /Whole workflow|publish_badge/ }),
      ).toHaveCount(0);
      await page.screenshot({
        path: test.info().outputPath(`curated-${theme}-${width}.png`),
        fullPage: true,
      });
      await page.getByRole('button', { name: 'Run job coverage', exact: true }).click();
      expect((await sent(page)).filter((message) => message.type === 'launch')).toContainEqual({
        type: 'launch',
        request: {
          itemId: 'workflow:ws:.github/workflows/coverage.yml',
          options: { job: 'coverage', event: 'workflow_call', inputs: {} },
        },
      });
    }
  }
});

test('node management posts scheduling and capacity changes', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Nodes/ }).click();
  const card = page.locator('.node-card', { hasText: 'big-box' });
  await card.getByRole('radio', { name: 'Drain' }).click();
  await expect(card.getByRole('button', { name: 'Save' })).toBeDisabled();
  await card.getByLabel('Max concurrent jobs').fill('6');
  await card.getByLabel('Reserved memory (GiB)').fill('8');
  await card.getByLabel('Labels').fill('big, fast');
  await card.getByRole('button', { name: 'Save' }).click();
  const msgs = (await sent(page)).filter((m) => m.type === 'updateNode');
  expect(msgs).toEqual([
    {
      type: 'updateNode',
      nodeId: '01NODEBIGBOX00000000000000',
      patch: { schedulingMode: 'draining' },
    },
    {
      type: 'updateNode',
      nodeId: '01NODEBIGBOX00000000000000',
      patch: { maxConcurrency: 6, reserved: { cpus: 1, memoryMb: 8192 }, labels: ['big', 'fast'] },
    },
  ]);
  await page.screenshot({ path: `${shots}nodes.png`, fullPage: true });
});

test('worker rename preserves unsaved input during polling and sends no settings changes', async ({
  page,
}) => {
  await open(page);
  await page.getByRole('tab', { name: /Nodes/ }).click();
  const card = page.locator('.node-card', { has: page.getByRole('heading', { name: /big-box/ }) });
  const name = card.getByLabel('Worker name');
  await name.fill('bad name');
  await expect(card.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  await name.press('Enter');
  expect((await sent(page)).filter((m) => m.type === 'updateNode')).toEqual([]);
  await name.fill('laptop');
  await expect(card.getByText('Another worker already uses this name.')).toBeVisible();
  await name.fill('big-box-renamed');
  await push(page, { type: 'state', state: state() });
  await expect(name).toHaveValue('big-box-renamed');
  await card.getByRole('button', { name: 'Save', exact: true }).click();
  expect((await sent(page)).filter((m) => m.type === 'updateNode')).toEqual([
    {
      type: 'updateNode',
      nodeId: '01NODEBIGBOX00000000000000',
      patch: { name: 'big-box-renamed' },
    },
  ]);
  const updated = state();
  updated.nodes[0]!.name = 'big-box-renamed';
  await push(page, { type: 'state', state: updated });
  await expect(page.locator('.node-card').first().getByRole('heading')).toContainText(
    'big-box-renamed',
  );
  await expect(
    page.locator('.node-card').first().getByRole('button', { name: 'Save', exact: true }),
  ).toBeDisabled();
});

test('worker rename form fits wide and narrow panels in both themes', async ({ page }) => {
  for (const [theme, css] of [
    ['dark', THEME],
    ['light', LIGHT_THEME],
  ] as const) {
    for (const width of [1280, 520]) {
      await page.setViewportSize({ width, height: 1000 });
      const initial = state();
      initial.nodes = initial.nodes.slice(0, 1);
      initial.nodes[0]!.name = 'DESKTOP-JEL';
      await open(page, initial, css);
      await page.getByRole('tab', { name: /Nodes/ }).click();
      await page.getByLabel('Worker name').fill('big-box');
      await expect(page.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      ).toBe(true);
      await page.screenshot({
        path: `${shots}worker-rename-${theme}-${width}.png`,
        fullPage: true,
      });
    }
  }
});

test('native workers show Rust targets and native settings retain act management', async ({
  page,
}) => {
  const initial = state();
  initial.connection.version = '2.0.0-dev.1';
  initial.connection.build = {
    ...initial.connection.build!,
    runtime: 'rust',
    target: 'linux/amd64',
  };
  initial.settings!.updates!.controller = {
    ...initial.settings!.updates!.controller,
    version: '2.0.0-dev.1',
    runtime: 'rust',
    target: 'linux/amd64',
  };
  initial.nodes[0]!.info!.build = {
    id: 'native-fixture',
    version: '2.0.0-dev.1',
    sha256: null,
    managed: true,
    runtime: 'rust',
    target: 'linux/amd64',
    lastRollback: null,
  };
  await open(page, initial);
  await page.getByRole('tab', { name: /Nodes/ }).click();
  await expect(page.locator('.node-card', { hasText: 'big-box' })).toContainText(
    'Rust · linux/amd64',
  );
  await page.screenshot({ path: `${shots}native-nodes.png`, fullPage: true });
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await expect(page.getByLabel('act version override')).toBeVisible();
  await expect(page.getByLabel('Node.js version override')).toHaveCount(0);
  await page.screenshot({ path: `${shots}native-settings.png`, fullPage: true });
});

test('sharded runs show shards, a timeline and combined test results', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Jobs/ }).click();
  await expect(page.getByText('Sharded runs')).toBeVisible();
  await page.locator('tr', { hasText: '1/2 shards passed' }).click();
  await expect.poll(() => sent(page)).toContainEqual({ type: 'requestGroupTests', groupId: GROUP });
  await push(page, {
    type: 'groupTests',
    groupId: GROUP,
    summary: {
      total: 8,
      passed: 7,
      failed: 1,
      skipped: 0,
      timeMs: 12_000,
      failures: [
        {
          suite: 'suite3.test.js',
          name: 'checksum differs',
          message: 'Expected values to differ',
          shard: 2,
        },
      ],
    },
  });
  await expect(page.locator('.test-totals')).toContainText('7 passed');
  await expect(page.locator('.timeline-bar')).toHaveCount(2);
  await expect(page.getByText('checksum differs')).toBeVisible();
  await page.screenshot({ path: `${shots}group.png`, fullPage: true });
  await page.getByRole('button', { name: 'Retry 1 failed shard' }).click();
  expect(await sent(page)).toContainEqual({ type: 'retryGroup', groupId: GROUP });
  await page.locator('tr', { hasText: '2/2' }).click();
  await expect(page.getByText('Shard 2 of 2 in a sharded run.')).toBeVisible();
});

test('launcher can shard a task and prefer a worker', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: /Run/ }).click();
  await page.locator('.run-item', { hasText: 'test' }).first().click();
  await page.getByLabel('Shards').fill('4');
  await page.getByLabel('Worker').selectOption('big-box');
  await page.getByText('Prefer this worker, but allow others when it is busy').click();
  await page.getByRole('button', { name: 'Run', exact: true }).click();
  const launches = (await sent(page)).filter((m) => m.type === 'launch');
  expect(launches).toEqual([
    {
      type: 'launch',
      request: { itemId: 'task:ws:test', options: { node: '', prefer: ['big-box'], shards: 4 } },
    },
  ]);
});

test('shows a connection banner with working actions when offline', async ({ page }) => {
  await open(
    page,
    state({
      connection: {
        status: 'error',
        url: 'https://192.168.1.77:7300',
        user: null,
        role: null,
        clusterId: null,
        version: null,
        error: 'Cannot reach the controller (ECONNREFUSED)',
      },
    }),
  );
  await expect(page.locator('.banner-error')).toContainText('ECONNREFUSED');
  await page.locator('.banner-error').getByRole('button', { name: 'Retry' }).click();
  await page.locator('.banner-error').getByRole('button', { name: 'Sign in' }).click();
  const msgs = await sent(page);
  expect(msgs).toContainEqual({ type: 'refresh' });
  expect(msgs).toContainEqual({ type: 'signIn' });
});

test('controller identity stays visible with and without a worker', async ({ page }) => {
  await open(page);
  await expect(page.locator('.controller-name')).toHaveText('Controller · laptop');
  await page.getByRole('tab', { name: 'Nodes', exact: true }).click();
  await expect(page.locator('.node-card', { hasText: 'laptop' })).toContainText(
    'Controller + worker',
  );
  await push(page, { type: 'state', state: state({ nodes: [] }) });
  await expect(page.locator('.controller-card')).toContainText('laptop');
  await expect(page.locator('.controller-card')).toContainText('Controller');
  await expect(page.getByText('No worker nodes yet')).toBeVisible();
});

test('settings save preferences and cluster configuration without losing drafts to live job updates', async ({
  page,
}) => {
  const initial = state();
  await open(page, initial);
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await expect.poll(() => sent(page)).toContainEqual({ type: 'refreshSettings' });
  await page.getByLabel('Recent job history').fill('150');
  await push(page, { type: 'state', state: { ...initial, jobs: initial.jobs.slice(1) } });
  await expect(page.getByLabel('Recent job history')).toHaveValue('150');
  await page.getByRole('button', { name: 'Save preferences', exact: true }).click();
  await page.getByLabel('Keep jobs (days)').fill('120');
  await page.getByLabel('Worker auto-updates', { exact: true }).selectOption('window');
  await page.getByLabel('Update window start').fill('23:00');
  await page.getByLabel('Update window end').fill('05:00');
  await page.getByRole('button', { name: 'Save cluster settings', exact: true }).click();
  const messages = await sent(page);
  expect(messages).toContainEqual({
    type: 'savePreferences',
    patch: { ...initial.settings!.preferences, jobHistory: 150 },
  });
  expect(messages).toContainEqual({
    type: 'saveClusterSettings',
    patch: {
      ...initial.settings!.cluster,
      jobRetentionDays: 120,
      autoUpdate: 'window',
      updateWindow: { start: '23:00', end: '05:00' },
    },
  });
});

test('settings create usable worker tokens from loopback connections and keep secrets out of persisted navigation', async ({
  page,
}) => {
  const initial = state();
  initial.connection = {
    ...initial.connection,
    url: 'https://127.0.0.1:7300',
    publicUrl: 'https://192.168.1.77:7300',
  };
  await open(page, initial);
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await page.getByLabel('Token name', { exact: true }).fill('build-worker');
  await page.getByLabel('Token type').selectOption('join');
  await page.getByRole('button', { name: 'Create token', exact: true }).click();
  expect(await sent(page)).toContainEqual({
    type: 'createToken',
    request: { kind: 'join', name: 'build-worker', role: 'operator', maxUses: 1 },
  });
  await push(page, {
    type: 'tokenCreated',
    token: 'gsj_fixture_secret',
    info: { ...initial.settings!.tokens![0]!, name: 'build-worker', kind: 'join' },
  });
  await expect(page.locator('.token-secret')).toContainText('gsj_fixture_secret');
  expect(
    await page.evaluate(() => JSON.stringify((window as unknown as { __saved: unknown }).__saved)),
  ).not.toContain('gsj_fixture_secret');
  await page.getByRole('button', { name: 'Copy token', exact: true }).click();
  expect(await sent(page)).toContainEqual({ type: 'copy', text: 'gsj_fixture_secret' });
  await page.getByRole('button', { name: 'Copy join command', exact: true }).click();
  expect(await sent(page)).toContainEqual({
    type: 'copy',
    text: `gridstator agent join 'https://192.168.1.77:7300' --fingerprint 'sha256:${'0'.repeat(64)}' --token 'gsj_fixture_secret'`,
  });
  await page.getByRole('button', { name: 'Dismiss token', exact: true }).click();
  await expect(page.locator('.token-secret')).toHaveCount(0);
  await page.getByTitle('Revoke developer-laptop').click();
  expect(await sent(page)).toContainEqual({ type: 'revokeToken', id: 'fixture-token' });
});

for (const appearance of [
  { name: 'dark', theme: THEME, width: 1280 },
  { name: 'light-narrow', theme: LIGHT_THEME, width: 390 },
]) {
  test(`add worker generates a desktop-compatible command and keeps it private (${appearance.name})`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: appearance.width, height: 900 });
    const initial = state();
    initial.connection.url = 'https://127.0.0.1:7400';
    initial.connection.publicUrl = 'https://192.168.1.77:7400';
    await open(page, initial, appearance.theme);
    await page.getByRole('tab', { name: 'Nodes', exact: true }).click();
    await page.getByRole('button', { name: 'Add worker', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Add a worker', exact: true })).toBeVisible();
    await expect(page.getByRole('tab', { name: 'Nodes', exact: true })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page.getByLabel('Controller URL for the worker')).toHaveValue(
      'https://192.168.1.77:7400',
    );
    await page.getByLabel('Command name', { exact: true }).fill('office-pc');
    await page.screenshot({
      path: `${shots}add-worker-form-${appearance.name}.png`,
      fullPage: true,
    });
    await page.getByRole('button', { name: 'Generate join command', exact: true }).click();
    expect(await sent(page)).toContainEqual({
      type: 'createWorkerJoinCommand',
      name: 'office-pc',
      url: 'https://192.168.1.77:7400',
    });
    await push(page, { type: 'busy', key: 'settings:tokens', busy: true });
    await expect(
      page.getByRole('button', { name: 'Generate join command', exact: true }),
    ).toBeDisabled();
    const command = `gridstator agent join 'https://192.168.1.77:7400' --fingerprint 'sha256:${'0'.repeat(64)}' --token 'gsj_private_join_secret'`;
    const result = {
      command,
      name: 'office-pc',
      clusterId: initial.connection.clusterId!,
      expiresAt: Date.now() + 86_400_000,
    };
    await push(page, { type: 'workerJoinCommandCreated', result });
    await push(page, { type: 'busy', key: 'settings:tokens', busy: false });
    const secret = page.getByRole('region', { name: 'Worker join command', exact: true });
    await expect(secret).toContainText(command);
    await expect(page.getByRole('heading', { name: 'Join command for office-pc' })).toBeFocused();
    await expect(page.getByText(/On Windows, select the WSL2 distribution/)).toBeVisible();
    await page.getByRole('button', { name: 'Copy join command', exact: true }).click();
    expect(await sent(page)).toContainEqual({ type: 'copy', text: command });
    expect(
      await page.evaluate(() =>
        JSON.stringify((window as unknown as { __saved: unknown }).__saved),
      ),
    ).not.toContain('gsj_private_join_secret');
    await page.screenshot({
      path: `${shots}add-worker-command-${appearance.name}.png`,
      fullPage: true,
    });
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
    ).toBe(true);
    await push(page, { type: 'state', state: { ...initial, jobs: [] } });
    await expect(secret).toContainText(command);
    await page.getByRole('button', { name: 'Generate another command', exact: true }).click();
    await expect(secret).toHaveCount(0);
    await expect(page.getByLabel('Command name', { exact: true })).toHaveValue('office-pc');
    await expect(page.getByLabel('Command name', { exact: true })).toBeFocused();
    await push(page, { type: 'workerJoinCommandCreated', result });
    await push(page, {
      type: 'state',
      state: { ...initial, connection: { ...initial.connection, clusterId: 'another-cluster' } },
    });
    await expect(secret).toHaveCount(0);
    await push(page, { type: 'workerJoinCommandCreated', result });
    await expect(secret).toHaveCount(0);
  });
}

test('add worker requires a reachable controller URL, a fingerprint and an admin connection', async ({
  page,
}) => {
  const initial = state();
  initial.connection.url = 'https://127.0.0.1:7300';
  delete initial.connection.publicUrl;
  await open(page, initial);
  // This is also the navigation used by the command palette and Cluster toolbar.
  await push(page, { type: 'navigate', view: 'addWorker' });
  await page.getByLabel('Command name', { exact: true }).fill('office-pc');
  const generate = page.getByRole('button', { name: 'Generate join command', exact: true });
  await expect(generate).toBeDisabled();
  await expect(page.getByLabel('Controller URL for the worker')).toHaveAttribute(
    'aria-invalid',
    'true',
  );
  await page.getByLabel('Controller URL for the worker').fill('https://build-server.local:7300');
  await expect(generate).toBeEnabled();
  await push(page, {
    type: 'state',
    state: { ...initial, connection: { ...initial.connection, caFingerprint: undefined } },
  });
  await expect(generate).toBeDisabled();
  await expect(page.getByText(/Reconnect to load the controller fingerprint/)).toBeVisible();
  await push(page, {
    type: 'state',
    state: { ...initial, connection: { ...initial.connection, role: 'operator' } },
  });
  await expect(generate).toBeDisabled();
  await expect(
    page.getByText('Sign in with admin access to generate a worker join command.'),
  ).toBeVisible();
  expect(
    (await sent(page)).filter((message) => message.type === 'createWorkerJoinCommand'),
  ).toHaveLength(0);
  await page.getByRole('button', { name: 'Back to nodes', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Add worker', exact: true })).toBeDisabled();
});

test('settings post release, safe rollout and local host actions', async ({ page }) => {
  await open(page);
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await page.getByRole('button', { name: 'Check for releases', exact: true }).click();
  await page.getByRole('button', { name: 'Install selected release', exact: true }).click();
  await page.getByRole('button', { name: 'Update workers', exact: true }).click();
  await page.getByRole('button', { name: 'Stop controller', exact: true }).click();
  await page.getByLabel('Listening port').fill('7400');
  await page.getByLabel('Controller URL for workers').fill('https://192.168.1.77:7400');
  await page.getByRole('button', { name: 'Apply & restart', exact: true }).click();
  const messages = await sent(page);
  expect(messages).toContainEqual({ type: 'requestReleases', includePrereleases: false });
  expect(messages).toContainEqual({ type: 'installRelease', tag: 'v0.2.0' });
  expect(messages).toContainEqual({
    type: 'startRollout',
    request: { now: false, drainTimeoutSeconds: null },
  });
  expect(messages).toContainEqual({ type: 'hostService', action: 'stop' });
  expect(messages).toContainEqual({
    type: 'saveHostConfiguration',
    host: '0.0.0.0',
    port: 7400,
    url: 'https://192.168.1.77:7400',
  });
});

test('offline settings remain available and offer explicit local recovery and service start', async ({
  page,
}) => {
  const initial = state();
  await open(page, {
    ...initial,
    connection: {
      ...initial.connection,
      status: 'error',
      error: 'Token revoked',
      canRestoreLocalAccess: true,
    },
    settings: {
      ...initial.settings!,
      cluster: null,
      tokens: null,
      host: {
        ...initial.settings!.host!,
        service: { installed: true, state: 'inactive', scope: 'user', canManage: true },
      },
    },
  });
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await expect(page.getByLabel('Recent job history')).toBeEnabled();
  await page
    .locator('#settings-connection')
    .getByRole('button', { name: 'Restore local access', exact: true })
    .click();
  await page.getByRole('button', { name: 'Start controller', exact: true }).click();
  expect(await sent(page)).toContainEqual({ type: 'restoreLocalAccess' });
  expect(await sent(page)).toContainEqual({ type: 'hostService', action: 'start' });
  await expect(page.getByRole('button', { name: 'Create token', exact: true })).toBeDisabled();
});

test('operator settings show versions and preferences while disabling cluster administration', async ({
  page,
}) => {
  const initial = state();
  await open(page, {
    ...initial,
    connection: { ...initial.connection, role: 'operator', canRestoreLocalAccess: false },
    settings: {
      ...initial.settings!,
      tokens: null,
      host: {
        ...initial.settings!.host!,
        available: false,
        reason: 'Host controls are available on the controller host.',
      },
    },
  });
  await page.getByRole('tab', { name: 'Settings', exact: true }).click();
  await expect(page.getByLabel('Recent job history')).toBeEnabled();
  await expect(page.getByLabel('Keep jobs (days)')).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Create token', exact: true })).toBeDisabled();
  await expect(
    page.getByRole('button', { name: 'Install selected release', exact: true }),
  ).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Update workers', exact: true })).toBeDisabled();
  await expect(page.getByRole('table', { name: 'Component versions' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Stop controller', exact: true })).toHaveCount(0);
});

for (const theme of ['dark', 'light'] as const) {
  for (const viewport of ['desktop', 'narrow'] as const) {
    test(`settings render without page overflow in ${theme} ${viewport}`, async ({ page }) => {
      await page.setViewportSize(
        viewport === 'desktop' ? { width: 1280, height: 860 } : { width: 390, height: 844 },
      );
      const errors: string[] = [];
      page.on('pageerror', (error) => errors.push(error.message));
      await open(page, state(), theme === 'dark' ? THEME : LIGHT_THEME);
      await page.getByRole('tab', { name: 'Settings', exact: true }).click();
      await expect(page.locator('.controller-name')).toBeVisible();
      await expect(page.getByRole('heading', { name: 'Connection', exact: true })).toBeVisible();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      ).toBe(true);
      await page.screenshot({ path: `${shots}settings-${theme}-${viewport}.png`, fullPage: true });
      expect(errors).toEqual([]);
    });
  }
}

test('first run offers local setup and an existing controller without an error banner', async ({
  page,
}) => {
  const initial = state();
  await open(page, {
    ...initial,
    connection: {
      ...initial.connection,
      status: 'signedOut',
      controller: undefined,
      user: null,
      role: null,
      clusterId: null,
      version: null,
      url: null,
      error: null,
      canSetUpController: true,
    },
    nodes: [],
    jobs: [],
    groups: [],
  });
  await expect(page.getByRole('heading', { name: 'Connect your machines' })).toBeVisible();
  await expect(page.locator('.banner-error')).toHaveCount(0);
  await page.getByRole('button', { name: 'Set up a controller here' }).click();
  await page.getByRole('button', { name: 'Connect to an existing controller' }).click();
  expect(await sent(page)).toContainEqual({ type: 'setupController' });
  expect(await sent(page)).toContainEqual({ type: 'signIn' });
  await page.screenshot({ path: `${shots}first-run-dark.png`, fullPage: true });
  await push(page, { type: 'busy', key: 'setup', busy: true });
  await expect(page.getByRole('button', { name: 'Setting up controller…' })).toBeDisabled();
  await expect(
    page.getByRole('button', { name: 'Connect to an existing controller' }),
  ).toBeDisabled();
  await page.setViewportSize({ width: 620, height: 860 });
  await page.screenshot({ path: `${shots}first-run-busy-narrow.png`, fullPage: true });
  await open(
    page,
    {
      ...initial,
      connection: {
        ...initial.connection,
        status: 'signedOut',
        controller: undefined,
        user: null,
        role: null,
        clusterId: null,
        version: null,
        url: null,
        error: null,
        canSetUpController: true,
      },
      nodes: [],
      jobs: [],
      groups: [],
    },
    LIGHT_THEME,
  );
  await page.screenshot({ path: `${shots}first-run-light-narrow.png`, fullPage: true });
});

test('stopped local controller offers Start without recreating it', async ({ page }) => {
  const initial = state();
  await open(page, {
    ...initial,
    connection: {
      ...initial.connection,
      status: 'error',
      error: 'Controller is stopped',
      canStartLocalController: true,
    },
  });
  await page.locator('.banner').getByRole('button', { name: 'Start controller' }).click();
  expect(await sent(page)).toContainEqual({ type: 'hostService', action: 'start' });
  await expect(page.getByRole('button', { name: 'Set up a controller here' })).toHaveCount(0);
});

test('interrupted setup offers a resumable action instead of another login', async ({ page }) => {
  const initial = state();
  await open(page, {
    ...initial,
    connection: {
      ...initial.connection,
      status: 'error',
      canSetUpController: true,
      setupPending: true,
      error: 'Service installation did not finish',
    },
    nodes: [],
    jobs: [],
    groups: [],
  });
  await expect(page.getByRole('status').filter({ hasText: 'setup hasn’t finished' })).toBeVisible();
  await page.getByRole('button', { name: 'Finish controller setup' }).click();
  expect(await sent(page)).toContainEqual({ type: 'setupController' });
  await expect(page.getByRole('button', { name: 'Sign in', exact: true })).toHaveCount(0);
});

test('a controller stays usable while its worker waits for Docker', async ({ page }) => {
  const initial = state();
  const worker = initial.nodes[0]!;
  await open(page, {
    ...initial,
    nodes: [
      {
        ...worker,
        info: {
          ...worker.info!,
          machineId: initial.connection.controller!.machineId,
          docker: { available: false },
          executors: [],
        },
      },
    ],
  });
  await expect(page.getByRole('status').filter({ hasText: 'waiting for Docker' })).toBeVisible();
  await expect(page.getByRole('tab', { name: 'Settings', exact: true })).toBeEnabled();
});

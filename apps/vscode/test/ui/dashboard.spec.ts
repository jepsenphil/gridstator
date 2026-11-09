import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import type { FromWebview, ToWebview } from '../../src/messages';
import { FAILED, RUNNING, state } from './fixtures';

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

async function open(page: Page, initial = state()): Promise<void> {
  const file = join(shots, '..', '.fixture.html');
  writeFileSync(
    file,
    `<!doctype html><html><head><meta charset="utf-8"><style>${THEME}</style>
     <link rel="stylesheet" href="file://${media}codicons/codicon.css"><link rel="stylesheet" href="file://${media}dashboard.css"></head>
     <body><div id="root"></div>
     <script>window.__sent = []; window.acquireVsCodeApi = () => ({ postMessage: (m) => window.__sent.push(m), getState: () => undefined, setState: () => {} });</script>
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
  await expect(page.locator('tbody tr')).toHaveCount(1);
  await page.getByTitle('Retry (same snapshot)').click();
  await page.getByRole('tab', { name: /^All/ }).click();
  await expect(page.locator('tbody tr')).toHaveCount(4);
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

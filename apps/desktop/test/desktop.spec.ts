import { mkdir } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';
import type { Snapshot } from '../src/api.js';
import { snapshot, worker } from './fixtures.js';

const captures = 'test/screenshots';

async function open(page: Page, initial = snapshot, failAction = false) {
  // Mock only the native command transport; the production bundle, forms and handlers run intact.
  await page.addInitScript(
    ({ initial, failAction }) => {
      const state = initial;
      const requests: { command: string; request?: Record<string, unknown> }[] = [];
      Object.assign(window, {
        __requests: requests,
        __TAURI_INTERNALS__: {
          invoke: async (command: string, args?: { request?: Record<string, unknown> }) => {
            const request = args?.request;
            requests.push({ command, request });
            if (command === 'desktop_snapshot') return structuredClone(state);
            if (command === 'desktop_logs')
              return {
                jobId: request?.jobId,
                entries: request?.afterSeq
                  ? []
                  : [
                      { s: 1, t: 1, o: 'stdout', d: 'Connecting with token=***\n' },
                      { s: 2, t: 2, o: 'stdout', d: '12 tests passed\n' },
                    ],
                nextSeq: 2,
              };
            if (command === 'desktop_action') {
              if (failAction)
                throw new Error('The local worker is unavailable. Start its service and retry.');
              const worker = state.workers.find((worker) => worker.id === request?.workerId)!;
              if (request?.action === 'pause') {
                worker.paused = true;
                worker.state = 'paused';
              }
              if (request?.action === 'resume') {
                worker.paused = false;
                worker.state = 'accepting';
              }
              if (request?.action === 'start') {
                worker.running = true;
                worker.state = worker.paused ? 'paused' : 'accepting';
              }
              if (request?.action === 'stop') {
                worker.draining = true;
                worker.state = 'draining';
              }
              if (request?.action === 'forceStop') {
                worker.running = false;
                worker.draining = false;
                worker.state = 'stopped';
                worker.activeJobs = [];
              }
              if (request?.action === 'cancel')
                worker.activeJobs = worker.activeJobs.filter((job) => job.id !== request.jobId);
              if (request?.action === 'setBudget') {
                worker.budget = request.budget as typeof worker.budget;
                worker.generation++;
              }
              if (request?.action === 'setStartup')
                worker.startAtSignIn = Boolean(request.startAtSignIn);
              return structuredClone(state);
            }
            if (command === 'desktop_settings') {
              state.settings = { ...state.settings, ...request };
              if (request?.nativeCliPath) state.nativeAvailable = true;
              return structuredClone(state);
            }
            if (command === 'desktop_install') {
              state.nativeAvailable = true;
              return structuredClone(state);
            }
            if (command === 'desktop_setup') return structuredClone(state);
            throw new Error(`Unexpected desktop command: ${command}`);
          },
        },
      });
    },
    { initial: structuredClone(initial), failAction },
  );
  await page.goto('/');
  await expect(
    page.getByRole('heading', {
      name: initial.workers.length ? initial.workers[0]!.name : 'Put this machine to work',
      exact: true,
    }),
  ).toBeVisible();
}

async function requests(page: Page) {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __requests: { command: string; request?: Record<string, unknown> }[];
        }
      ).__requests,
  );
}

test('pause is persistent local intent; resume leaves controller hold visible', async ({
  page,
}) => {
  const initial: Snapshot = {
    ...snapshot,
    workers: [{ ...worker, controllerSettings: { schedulingMode: 'draining' } }],
  };
  await open(page, initial);
  await expect(page.getByText('Held by controller')).toBeVisible();
  await page.getByRole('button', { name: 'Pause worker', exact: true }).click();
  await expect(page.getByText('Paused locally', { exact: true })).toBeVisible();
  await expect(page.getByText('Paused until you resume')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Cancel Run integration tests' })).toBeVisible();
  await page.getByRole('button', { name: 'Resume worker', exact: true }).click();
  await expect(page.getByText('Held by controller')).toBeVisible();
  await expect(page.getByText('Scheduling draining')).toBeVisible();
  expect(
    (await requests(page))
      .filter((entry) => entry.command === 'desktop_action')
      .map((entry) => entry.request?.action),
  ).toEqual(['pause', 'resume']);
});

test('graceful stop preserves jobs and controller; force stop requires confirmation', async ({
  page,
}) => {
  await open(page);
  await page.getByRole('button', { name: 'Stop worker', exact: true }).click();
  await expect(page.getByText('Stopping after jobs finish')).toBeVisible();
  await expect(
    page.getByRole('button', { name: 'Read logs for Run integration tests' }),
  ).toBeVisible();
  await expect(page.getByText('Running independently')).toBeVisible();
  await page.getByRole('button', { name: 'Force-stop…', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('all 2 running jobs');
  expect(
    (await requests(page)).filter((entry) => entry.request?.action === 'forceStop'),
  ).toHaveLength(0);
  await page.getByRole('button', { name: 'Keep running' }).click();
  await page.getByRole('button', { name: 'Force-stop…', exact: true }).click();
  await page.getByRole('button', { name: 'Force-stop worker', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Start worker', exact: true })).toBeVisible();
  await expect(page.getByText('Running independently')).toBeVisible();
  expect(await requests(page)).toContainEqual({
    command: 'desktop_action',
    request: { workerId: worker.id, action: 'forceStop', confirmed: true },
  });
});

test('cancel applies only to the confirmed local job', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: 'Cancel Run integration tests', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText(
    'Other jobs and the controller keep running',
  );
  await page.getByRole('button', { name: 'Cancel job', exact: true }).click();
  await expect(
    page.getByRole('button', { name: 'Read logs for Run integration tests' }),
  ).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Read logs for Build app image' })).toBeVisible();
  expect(await requests(page)).toContainEqual({
    command: 'desktop_action',
    request: { workerId: worker.id, action: 'cancel', jobId: 'job-test-suite', confirmed: true },
  });
});

test('admission budget and startup settings invoke local typed controls', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: 'Resources & startup' }).click();
  await page.getByLabel('CPU capacity').fill('2.5');
  await page.getByLabel('Memory capacity').fill('4096');
  await page.getByLabel('Concurrent jobs').fill('1');
  await page.getByRole('button', { name: 'Save local budget' }).click();
  await expect(
    page.getByText('These are admission limits, not a machine usage cap.'),
  ).toBeVisible();
  await expect(page.getByRole('table')).toContainText('2 cores reserved');
  await expect(page.getByRole('table')).toContainText('6 cores');
  expect(await requests(page)).toContainEqual({
    command: 'desktop_action',
    request: {
      workerId: worker.id,
      action: 'setBudget',
      budget: { cpus: 2.5, memoryMb: 4096, maxConcurrency: 1 },
    },
  });
  await page.getByLabel('Start this worker at OS sign-in').uncheck();
  await expect(page.getByLabel('Start this worker at OS sign-in')).not.toBeChecked();
  await page.getByLabel('Open the companion at OS sign-in').uncheck();
  expect(await requests(page)).toContainEqual({
    command: 'desktop_settings',
    request: { startAtSignIn: false },
  });
});

test('local controls remain visible offline and action errors recover without lying about state', async ({
  page,
}) => {
  await open(
    page,
    { ...snapshot, workers: [{ ...worker, connected: false, state: 'disconnected' }] },
    true,
  );
  await expect(page.getByText('Unknown while disconnected')).toBeVisible();
  await page.getByRole('button', { name: 'Pause worker', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Start its service and retry');
  await expect(page.getByRole('button', { name: 'Pause worker', exact: true })).toBeEnabled();
  await expect(page.getByText('Controller disconnected', { exact: true })).toBeVisible();
});

test('logs show worker-masked output as text, not markup', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: 'Read logs for Run integration tests' }).click();
  await expect(page.getByLabel('Logs for Run integration tests', { exact: true })).toContainText(
    'token=***',
  );
  await expect(page.getByLabel('Logs for Run integration tests', { exact: true })).toContainText(
    '12 tests passed',
  );
  expect(await requests(page)).toContainEqual({
    command: 'desktop_logs',
    request: { workerId: worker.id, jobId: 'job-test-suite', limit: 500 },
  });
});

test('Windows onboarding selects WSL2 and clears a pasted join credential before invoking', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    nativeAvailable: false,
    wslDistributions: [
      { name: 'Ubuntu', version: 2, state: 'stopped' },
      { name: 'Legacy', version: 1, state: 'stopped' },
    ],
  });
  await expect(page.getByLabel('WSL2 distribution')).toHaveValue('Ubuntu');
  await expect(page.getByRole('option', { name: 'Legacy · WSL1 — unsupported' })).toHaveJSProperty(
    'disabled',
    true,
  );
  await page
    .getByLabel('Worker join command', { exact: true })
    .fill(
      'gridstator agent join https://controller.example:7443 --token join-test-secret --ca-fingerprint abc',
    );
  await page.getByRole('button', { name: 'Join controller', exact: true }).click();
  await expect(page.getByLabel('Worker join command', { exact: true })).toHaveValue('');
  expect(
    (await requests(page)).find((entry) => entry.command === 'desktop_setup')?.request,
  ).toMatchObject({
    mode: 'join',
    distribution: 'Ubuntu',
    connection:
      'gridstator agent join https://controller.example:7443 --token join-test-secret --ca-fingerprint abc',
  });
  expect(
    await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } })),
  ).toEqual({ local: {}, session: {} });
});

test('manual join validates HTTPS; create never asks for login credentials', async ({ page }) => {
  await open(page, { ...snapshot, workers: [] });
  await page.getByRole('button', { name: 'Enter connection details manually' }).click();
  await page.getByLabel('Controller URL', { exact: true }).fill('http://controller.example');
  await page.getByLabel('Controller CA fingerprint', { exact: true }).fill('abc');
  await page.getByLabel('Worker join token', { exact: true }).fill('join-test');
  await page.getByRole('button', { name: 'Join controller', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Use an HTTPS controller URL');
  await page.getByLabel('Create a controller here').check();
  await expect(page.getByLabel('Worker join token', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Create controller & worker' }).click();
  expect(
    (await requests(page)).find((entry) => entry.command === 'desktop_setup')?.request,
  ).toEqual({ mode: 'create' });
});

test('missing WSL guides user installation and never starts setup', async ({ page }) => {
  await open(page, { ...snapshot, platform: 'windows', workers: [], wslDistributions: [] });
  await expect(page.getByText('A WSL2 distribution is needed.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Join controller', exact: true })).toBeDisabled();
  expect((await requests(page)).some((entry) => entry.command === 'desktop_setup')).toBe(false);
});

test('an installed helper can be selected and automatic discovery restored without overwriting it', async ({
  page,
}) => {
  await open(page, { ...snapshot, workers: [], nativeAvailable: false });
  await page.getByLabel('Native CLI path', { exact: true }).fill('relative/gridstator');
  await page.getByRole('button', { name: 'Use this CLI', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Enter an absolute path');
  await page.getByLabel('Native CLI path', { exact: true }).fill('/opt/gridstator/bin/gridstator');
  await page.getByRole('button', { name: 'Use this CLI', exact: true }).click();
  expect(await requests(page)).toContainEqual({
    command: 'desktop_settings',
    request: { nativeCliPath: '/opt/gridstator/bin/gridstator' },
  });
  await expect(page.getByRole('button', { name: 'Install verified CLI now' })).toHaveCount(0);
  await page.getByText('Use an installed native CLI', { exact: true }).click();
  await page.getByRole('button', { name: 'Use automatic discovery', exact: true }).click();
  expect(await requests(page)).toContainEqual({
    command: 'desktop_settings',
    request: { nativeCliPath: '' },
  });
});

test('waiting for Docker is a completed setup state with usable local controls', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    workers: [{ ...worker, state: 'waitingForDocker', connected: false, activeJobs: [] }],
  });
  await expect(page.getByText('Waiting for Docker', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Pause worker', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Stop worker', exact: true })).toBeEnabled();
});

test('dark, light and narrow worker windows fit their viewport', async ({ page }) => {
  await mkdir(captures, { recursive: true });
  await open(page);
  await page.getByRole('button', { name: 'Read logs for Run integration tests' }).click();
  await expect(page.getByLabel('Logs for Run integration tests', { exact: true })).toContainText(
    '12 tests passed',
  );
  await page.screenshot({ path: `${captures}/activity-dark.png`, fullPage: true });
  await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
  await page.getByRole('button', { name: 'Resources & startup' }).click();
  await page.screenshot({ path: `${captures}/resources-light.png`, fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.getByRole('button', { name: /^Activity/ }).click();
  await page.screenshot({ path: `${captures}/activity-narrow.png`, fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await page.getByRole('button', { name: 'Add worker', exact: true }).click();
  await page.screenshot({ path: `${captures}/setup-narrow.png`, fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
});

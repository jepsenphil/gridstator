import { mkdir } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';
import type { PrerequisiteStatus, Snapshot } from '../src/api.js';
import { snapshot, worker } from './fixtures.js';

const captures = 'test/screenshots';

async function open(page: Page, initial = snapshot, failAction = false) {
  // Mock only the native command transport; the production bundle, forms and handlers run intact.
  await page.addInitScript(
    ({ initial, failAction }) => {
      const state = initial;
      const hasWsl = state.wslDistributions.some((entry) => entry.version === 2);
      if (state.platform === 'windows' && !state.prerequisites) {
        state.prerequisites = {
          supported: true,
          revision: 'plan-one',
          phase: 'review',
          engine: hasWsl ? 'engine' : 'desktop',
          distribution: 'Gridstator-Ubuntu',
          linuxUser: 'gridstator',
          wslAvailable: hasWsl,
          existingWorker: false,
          ownedDistribution: false,
          message: 'Review this machine’s setup before installing prerequisites.',
          error: null,
          manualAction: null,
          dockerTermsRequired: !hasWsl,
          steps: ['WSL2', 'Ubuntu', 'Docker', 'Container access'].map((label, index) => ({
            id: String(index),
            label,
            state: 'pending' as const,
          })),
          distributions: state.wslDistributions,
          canRetry: false,
          canRestart: false,
          canDelete: false,
        };
      }
      const requests: { command: string; request?: Record<string, unknown> }[] = [];
      Object.assign(window, {
        __requests: requests,
        __desktopState: state,
        __TAURI_INTERNALS__: {
          invoke: async (command: string, args?: { request?: Record<string, unknown> }) => {
            const request = args?.request;
            requests.push({ command, request });
            if (command === 'desktop_snapshot') return structuredClone(state);
            if (command === 'desktop_prerequisites') {
              const preparation = state.prerequisites!;
              const gate = window as unknown as {
                __holdInspect?: boolean;
                __releaseInspect?: () => void;
              };
              if (request?.action === 'inspect' && gate.__holdInspect) {
                await new Promise<void>((resolve) => {
                  gate.__releaseInspect = resolve;
                });
              }

              if (request?.action === 'inspect') {
                if (request.engine) preparation.engine = request.engine as 'engine' | 'desktop';
                if (typeof request.distribution === 'string') {
                  preparation.distribution = request.distribution;
                  preparation.linuxUser =
                    preparation.distribution === 'Gridstator-Ubuntu'
                      ? 'gridstator'
                      : 'existing-user';
                  preparation.existingWorker = false;
                }
                if (request.engine || request.distribution) {
                  preparation.revision += '-changed';
                  preparation.phase = 'review';
                  preparation.dockerTermsRequired = preparation.engine === 'desktop';
                }
              }
              if (
                request?.action === 'start' ||
                request?.action === 'retry' ||
                request?.action === 'verify'
              ) {
                preparation.phase = 'ready';
                preparation.message = 'The container runtime is ready.';
                preparation.dockerTermsRequired = false;
                preparation.steps.forEach((step) => {
                  step.state = 'done';
                });
              }
              if (request?.action === 'delete') {
                preparation.canDelete = false;
                preparation.ownedDistribution = false;
                preparation.phase = 'review';
                preparation.message = 'The dedicated environment was removed.';
              }
              return structuredClone(preparation);
            }
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
  await expect(page.getByLabel('Ubuntu environment')).toHaveValue('Gridstator-Ubuntu');
  await expect(page.getByRole('option', { name: /Legacy/ })).toHaveCount(0);
  await page.getByRole('button', { name: 'Install and prepare', exact: true }).click();
  await page.getByRole('button', { name: 'Continue to connection', exact: true }).click();
  await expect(page.getByLabel('Linux account')).toHaveCount(0);
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
    distribution: 'Gridstator-Ubuntu',
    linuxUser: 'gridstator',
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

test('missing WSL defaults Desktop and requires review before installation', async ({ page }) => {
  await open(page, { ...snapshot, platform: 'windows', workers: [], wslDistributions: [] });
  await expect(page.getByRole('radio', { name: /^Docker Desktop/ })).toBeChecked();
  await expect(page.getByRole('button', { name: 'Install and prepare' })).toBeDisabled();
  await expect(page.getByLabel('Worker join command')).toHaveCount(0);
  expect((await requests(page)).some((entry) => entry.command === 'desktop_setup')).toBe(false);
  await page.getByRole('checkbox', { name: /I accept the Docker Subscription/ }).check();
  await page.getByRole('button', { name: 'Install and prepare' }).click();
  expect(
    (await requests(page)).find((entry) => entry.request?.action === 'start')?.request,
  ).toMatchObject({
    revision: 'plan-one',
    engine: 'desktop',
    distribution: 'Gridstator-Ubuntu',
    confirmed: true,
    acceptDockerTerms: true,
  });
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

function preparation(overrides: Partial<PrerequisiteStatus> = {}): PrerequisiteStatus {
  return {
    supported: true,
    revision: 'reviewed-plan',
    phase: 'review',
    engine: 'engine',
    distribution: 'Gridstator-Ubuntu',
    linuxUser: 'gridstator',
    wslAvailable: true,
    existingWorker: false,
    ownedDistribution: false,
    dockerTermsRequired: false,
    message: 'Review this machine’s setup before installing prerequisites.',
    error: null,
    manualAction: null,
    canRetry: false,
    canRestart: false,
    canDelete: false,
    distributions: [{ name: 'Ubuntu', version: 2, state: 'running' }],
    steps: ['Prepare WSL2', 'Prepare Ubuntu', 'Prepare Docker', 'Verify container access'].map(
      (label, index) => ({
        id: String(index),
        label,
        state: 'pending',
      }),
    ),
    ...overrides,
  };
}

async function changePreparation(page: Page, changes: Partial<PrerequisiteStatus>) {
  await page.evaluate((changes) => {
    const state = (window as unknown as { __desktopState: Snapshot }).__desktopState;
    Object.assign(state.prerequisites!, changes);
  }, changes);
}

test('working Desktop remains selected when WSL2 is already available; review is responsive', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({ engine: 'desktop', dockerTermsRequired: false }),
  });
  await expect(page.getByRole('radio', { name: /^Docker Desktop/ })).toBeChecked();
  await expect(page.getByLabel('Ubuntu environment')).toHaveValue('Gridstator-Ubuntu');
  await expect(page.getByLabel('Linux account')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Install and prepare' })).toBeEnabled();
  await page.screenshot({ path: `${captures}/windows-setup-review-dark.png`, fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ colorScheme: 'light' });
  await page.screenshot({ path: `${captures}/windows-setup-review-narrow.png`, fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('background progress is polled and a saved restart still requires explicit confirmation', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({
      phase: 'running',
      engine: 'desktop',
      message: 'Installing Ubuntu…',
    }),
  });
  await expect(page.getByText('Installing Ubuntu…')).toBeVisible();
  await changePreparation(page, {
    message: 'Preparing Docker Desktop…',
    steps: preparation().steps.map((step, index) => ({
      ...step,
      state: index < 2 ? 'done' : index === 2 ? 'running' : 'pending',
    })),
  });
  await expect(page.getByText('Preparing Docker Desktop…')).toBeVisible();
  await page.screenshot({ path: `${captures}/windows-setup-progress.png`, fullPage: true });
  await changePreparation(page, {
    phase: 'restartRequired',
    canRestart: true,
    message: 'Restart Windows when convenient. Your selected Docker Desktop setup will resume.',
  });
  await expect(page.getByRole('button', { name: 'Restart Windows…', exact: true })).toBeVisible();
  await page.screenshot({ path: `${captures}/windows-setup-restart.png`, fullPage: true });
  expect((await requests(page)).some((entry) => entry.request?.action === 'restart')).toBe(false);
  await page.getByRole('button', { name: 'Restart Windows…', exact: true }).click();
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect((await requests(page)).some((entry) => entry.request?.action === 'restart')).toBe(false);
  await page.getByRole('button', { name: 'Restart Windows…', exact: true }).click();
  await page.getByRole('button', { name: 'Restart Windows now', exact: true }).click();
  expect(
    (await requests(page)).find((entry) => entry.request?.action === 'restart')?.request,
  ).toMatchObject({
    engine: 'desktop',
    revision: 'reviewed-plan',
    confirmed: true,
  });
});

test('reopening a failed setup retains the engine and retries its reviewed plan', async ({
  page,
}) => {
  const initial: Snapshot = {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({
      phase: 'failed',
      engine: 'desktop',
      canRetry: true,
      message: 'Your progress has been retained.',
      error: 'The download failed. Check your connection and retry.',
    }),
  };
  await open(page, initial);
  await page.reload();
  await expect(page.getByRole('radio', { name: /^Docker Desktop/ })).toBeChecked();
  await expect(page.getByRole('alert')).toContainText('The download failed');
  await page.screenshot({ path: `${captures}/windows-setup-failed.png`, fullPage: true });
  await page.getByRole('button', { name: 'Retry setup', exact: true }).click();
  expect(
    (await requests(page)).find((entry) => entry.request?.action === 'retry')?.request,
  ).toMatchObject({
    engine: 'desktop',
    distribution: 'Gridstator-Ubuntu',
    revision: 'reviewed-plan',
    confirmed: true,
  });
});

test('Desktop manual fallback verifies integration before collecting controller credentials', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({
      phase: 'actionRequired',
      engine: 'desktop',
      canRetry: true,
      manualAction:
        'Enable Gridstator-Ubuntu in Docker Desktop > Settings > Resources > WSL integration.',
    }),
  });
  await expect(page.getByLabel('Worker join command')).toHaveCount(0);
  await page.getByRole('button', { name: 'Open Docker Desktop', exact: true }).click();
  expect((await requests(page)).some((entry) => entry.request?.action === 'openDocker')).toBe(true);
  await page.getByRole('button', { name: 'Verify and continue', exact: true }).click();
  await page.getByRole('button', { name: 'Continue to connection', exact: true }).click();
  await expect(page.getByLabel('Worker join command')).toBeVisible();
  expect((await requests(page)).some((entry) => entry.command === 'desktop_setup')).toBe(false);
});

test('existing workers keep their dashboard and can return after an environment check', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    prerequisites: preparation({
      phase: 'ready',
      existingWorker: true,
      distribution: 'Ubuntu',
      linuxUser: 'existing-user',
    }),
  });
  expect((await requests(page)).some((entry) => entry.command === 'desktop_prerequisites')).toBe(
    false,
  );
  await page.getByRole('button', { name: 'Check setup', exact: true }).click();
  await expect(page.getByText('Your existing worker is preserved.')).toBeVisible();
  await page.getByRole('button', { name: 'Return to worker', exact: true }).click();
  await expect(page.getByRole('heading', { name: worker.name, exact: true })).toBeVisible();
  expect((await requests(page)).some((entry) => entry.command === 'desktop_setup')).toBe(false);
});

test('owned environment deletion requires explicit data-loss confirmation', async ({ page }) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({ phase: 'ready', ownedDistribution: true, canDelete: true }),
  });
  await page.getByText('Remove the dedicated environment', { exact: true }).click();
  await page.getByRole('button', { name: 'Delete environment and data…', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('Permanently delete Gridstator-Ubuntu');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect((await requests(page)).some((entry) => entry.request?.action === 'delete')).toBe(false);
  await page.getByRole('button', { name: 'Delete environment and data…', exact: true }).click();
  await page.getByRole('button', { name: 'Delete environment and data', exact: true }).click();
  expect(
    (await requests(page)).find((entry) => entry.request?.action === 'delete')?.request,
  ).toMatchObject({
    distribution: 'Gridstator-Ubuntu',
    revision: 'reviewed-plan',
    confirmed: true,
  });
});

test('a confirmed restart waits for an in-flight status poll instead of being dropped', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({ phase: 'restartRequired', engine: 'desktop', canRestart: true }),
  });
  await expect(page.getByRole('button', { name: 'Restart Windows…', exact: true })).toBeEnabled();
  await page.evaluate(() => {
    (window as unknown as { __holdInspect: boolean }).__holdInspect = true;
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        Boolean((window as unknown as { __releaseInspect?: () => void }).__releaseInspect),
      ),
    )
    .toBe(true);
  await page.getByRole('button', { name: 'Restart Windows…', exact: true }).click();
  await page.getByRole('button', { name: 'Restart Windows now', exact: true }).click();
  await page.evaluate(() => {
    const gate = window as unknown as { __holdInspect: boolean; __releaseInspect: () => void };
    gate.__holdInspect = false;
    gate.__releaseInspect();
  });
  await expect
    .poll(
      async () =>
        (await requests(page)).filter((entry) => entry.request?.action === 'restart').length,
    )
    .toBe(1);
});

test('blocked Desktop integration offers an explicit engine change without automatic fallback', async ({
  page,
}) => {
  await open(page, {
    ...snapshot,
    platform: 'windows',
    workers: [],
    prerequisites: preparation({
      phase: 'actionRequired',
      engine: 'desktop',
      canRetry: true,
      manualAction: 'Your organization must enable WSL integration, or choose Docker Engine.',
    }),
  });
  expect((await requests(page)).some((entry) => entry.request?.action === 'start')).toBe(false);
  await page.getByRole('button', { name: 'Change setup choices', exact: true }).click();
  await page.getByRole('radio', { name: /^Docker Engine/ }).check();
  await expect(page.getByRole('radio', { name: /^Docker Engine/ })).toBeChecked();
  await page.getByRole('button', { name: 'Install and prepare', exact: true }).click();
  expect(
    (await requests(page)).find((entry) => entry.request?.action === 'start')?.request,
  ).toMatchObject({
    engine: 'engine',
    distribution: 'Gridstator-Ubuntu',
    confirmed: true,
  });
});

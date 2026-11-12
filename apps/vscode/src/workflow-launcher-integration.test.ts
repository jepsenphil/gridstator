import type * as vscode from 'vscode';
import type * as Discovery from '@gridstator/discovery';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseWorkflow } from '@gridstator/discovery';
import { JobSpecSchema, ProjectConfigSchema, type Job } from '@gridstator/protocol';
import { JobSecrets } from './job-secrets';
import { Launcher } from './launcher';
import type { NativeCli } from './native';
import type { ClusterState } from './state';

const mocks = vi.hoisted(() => ({
  prompt: vi.fn(),
  capture: vi.fn(),
  upload: vi.fn(),
  find: vi.fn(),
  config: vi.fn(),
  workflows: vi.fn(),
}));

vi.mock('vscode', async () => {
  const host = await import('./test-vscode');
  const disposable = { dispose() {} };
  return {
    ...host,
    workspace: {
      ...host.workspace,
      workspaceFolders: [{ name: 'project', uri: { scheme: 'file', fsPath: '/projects/app' } }],
      textDocuments: [],
      createFileSystemWatcher: () => ({
        ...disposable,
        onDidChange: () => disposable,
        onDidCreate: () => disposable,
        onDidDelete: () => disposable,
      }),
      onDidChangeWorkspaceFolders: () => disposable,
    },
    window: { ...host.window, showInputBox: mocks.prompt },
  };
});
vi.mock('@gridstator/snapshot', () => ({
  captureSnapshot: mocks.capture,
  uploadSnapshot: mocks.upload,
  findProject: mocks.find,
  loadProjectConfig: mocks.config,
}));
vi.mock('@gridstator/discovery', async (importOriginal) => ({
  ...(await importOriginal<typeof Discovery>()),
  discoverDockerfiles: async () => [],
  discoverPackageScripts: async () => [],
  discoverWorkflows: mocks.workflows,
}));

const secretName = 'GRIDSTATOR_CURATED_LAUNCH_TEST_SECRET';
const coverageFile = '.github/workflows/coverage.yml';
const hiddenFile = '.github/workflows/ci-cd-staging.yml';
const coverageId = `workflow:project:${coverageFile}`;
const hiddenId = `workflow:project:${hiddenFile}`;
const config = ProjectConfigSchema.parse({
  actions: {
    secretEnv: [secretName],
    launcher: {
      'coverage.yml': { description: 'Measure test coverage locally', jobs: ['coverage'] },
    },
  },
});
const workflows = [
  parseWorkflow(
    coverageFile,
    `name: 'Checks: Coverage'
on: workflow_call
jobs:
  coverage: { runs-on: ubuntu-latest, steps: [] }
  publish_badge: { runs-on: ubuntu-latest, needs: coverage, steps: [] }
`,
  ),
  parseWorkflow(
    hiddenFile,
    `name: 'Internal: Staging build'
on: workflow_call
jobs:
  staging: { uses: ./.github/workflows/ci-cd.yml }
`,
  ),
];
const launchers: Launcher[] = [];

function job(spec: Job['spec']): Job {
  return {
    id: 'submitted-job',
    name: 'coverage',
    kind: 'actions',
    state: 'queued',
    stateReason: null,
    priority: 0,
    owner: 'test',
    spec,
    snapshotId: null,
    nodeId: null,
    nodeName: null,
    assignmentId: null,
    attempt: 1,
    retryOf: null,
    exitCode: null,
    error: null,
    queuePosition: null,
    logSeq: 0,
    outputs: null,
    artifacts: [],
    groupId: null,
    shardIndex: null,
    shardTotal: null,
    createdAt: 1,
    queuedAt: 1,
    assignedAt: null,
    startedAt: null,
    finishedAt: null,
    cancelRequestedAt: null,
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.stubEnv(secretName, undefined);
  mocks.find.mockResolvedValue({
    root: '/projects/app',
    workdir: '.',
    configPath: '/projects/app/gridstator.yml',
  });
  mocks.config.mockResolvedValue(config);
  mocks.workflows.mockResolvedValue(workflows);
  mocks.capture.mockResolvedValue({
    id: `sha256:${'a'.repeat(64)}`,
    stats: { files: 1, totalBytes: 10 },
    warnings: [],
  });
  mocks.prompt.mockResolvedValue('fixture-curated-launch-token');
});

afterEach(() => {
  for (const launcher of launchers.splice(0)) launcher.dispose();
  vi.unstubAllEnvs();
});

async function setup(mode: 'node' | 'native') {
  const result = job(
    JobSpecSchema.parse({
      kind: 'actions',
      workflow: coverageFile,
      job: 'coverage',
      snapshot: `sha256:${'a'.repeat(64)}`,
      secretEnv: { [secretName]: '***' },
    }),
  );
  const client = { submitJob: vi.fn().mockResolvedValue(result) };
  const state = {
    requireClient: () => client,
    connection: { clusterId: 'test-cluster' },
    jobs: new Map<string, Job>(),
    myJobs: new Set<string>(),
    upsertJob: vi.fn(),
  };
  const native = {
    enabled: true,
    execute: vi.fn(async (request: { operation: string }) =>
      request.operation === 'discover'
        ? {
            root: '/projects/app',
            workdir: '.',
            config,
            workflows,
            packages: [],
            dockerfiles: [],
          }
        : result,
    ),
  };
  const storage = { get: vi.fn(), store: vi.fn(), delete: vi.fn() };
  const secrets = new JobSecrets(storage, async () => 'test-host');
  const resolveSecrets = vi.spyOn(secrets, 'resolve');
  const launcher = new Launcher(
    state as unknown as ClusterState,
    { appendLine: vi.fn() } as unknown as vscode.OutputChannel,
    mode === 'native' ? (native as unknown as NativeCli) : undefined,
    secrets,
  );
  launchers.push(launcher);
  await launcher.refresh();
  native.execute.mockClear();
  return { launcher, native, client, state, resolveSecrets };
}

describe.each(['node', 'native'] as const)('%s curated workflow launcher integration', (mode) => {
  it('exposes only the curated workflow and job with its explanatory description', async () => {
    const { launcher } = await setup(mode);
    expect(launcher.items).toEqual([
      expect.objectContaining({
        id: coverageId,
        description: 'Measure test coverage locally',
        workflow: expect.objectContaining({
          wholeWorkflow: false,
          jobs: [expect.objectContaining({ id: 'coverage' })],
        }),
      }),
    ]);
  });

  it.each([
    ['excluded stale selection', hiddenId, {}, 'NOT_FOUND'],
    ['internal badge job', coverageId, { job: 'publish_badge' }, 'BAD_REQUEST'],
    ['whole workflow', coverageId, {}, 'BAD_REQUEST'],
  ] as const)('rejects %s before credentials or submission', async (_, itemId, options, code) => {
    const { launcher, native, client, resolveSecrets } = await setup(mode);
    await expect(launcher.launch({ itemId, options })).rejects.toMatchObject({ code });
    expect(resolveSecrets).not.toHaveBeenCalled();
    expect(mocks.prompt).not.toHaveBeenCalled();
    expect(mocks.capture).not.toHaveBeenCalled();
    expect(mocks.upload).not.toHaveBeenCalled();
    expect(client.submitJob).not.toHaveBeenCalled();
    expect(native.execute).not.toHaveBeenCalled();
  });

  it('submits the chosen standalone check rather than its entire workflow', async () => {
    const { launcher, native, client, resolveSecrets } = await setup(mode);
    await launcher.launch({
      itemId: coverageId,
      options: { job: 'coverage', event: 'workflow_call' },
    });
    const expectedSpec = expect.objectContaining({
      kind: 'actions',
      workflow: coverageFile,
      job: 'coverage',
      event: 'workflow_call',
      secretEnv: { [secretName]: 'fixture-curated-launch-token' },
    });
    expect(resolveSecrets).toHaveBeenCalledTimes(1);
    if (mode === 'native') {
      expect(native.execute).toHaveBeenCalledWith(
        expect.objectContaining({ operation: 'submit', spec: expectedSpec }),
        expect.any(AbortSignal),
      );
      expect(mocks.capture).not.toHaveBeenCalled();
    } else {
      expect(client.submitJob).toHaveBeenCalledWith(expectedSpec);
      expect(mocks.capture).toHaveBeenCalledTimes(1);
      expect(mocks.upload).toHaveBeenCalledTimes(1);
    }
  });

  it.each([
    ['excluded workflow', hiddenFile, undefined],
    ['internal badge job', coverageFile, 'publish_badge'],
    ['whole workflow', coverageFile, undefined],
  ] as const)(
    'rejects rerunning an external %s before snapshot or submission',
    async (_, file, id) => {
      const { launcher, native, client, state, resolveSecrets } = await setup(mode);
      state.jobs.set(
        'external',
        job(
          JobSpecSchema.parse({
            kind: 'actions',
            workflow: file,
            ...(id ? { job: id } : {}),
            snapshot: `sha256:${'a'.repeat(64)}`,
          }),
        ),
      );
      await expect(launcher.rerun('external')).rejects.toMatchObject({ code: 'BAD_REQUEST' });
      expect(resolveSecrets).not.toHaveBeenCalled();
      expect(mocks.capture).not.toHaveBeenCalled();
      expect(mocks.upload).not.toHaveBeenCalled();
      expect(client.submitJob).not.toHaveBeenCalled();
      expect(native.execute).not.toHaveBeenCalled();
    },
  );
});

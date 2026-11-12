import type * as vscode from 'vscode';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { JobSpecSchema, ProjectConfigSchema, type Job } from '@gridstator/protocol';
import type { ClusterState } from './state';
import type { NativeCli } from './native';
import { JobSecrets, SecretInputCancelled } from './job-secrets';
import { Launcher } from './launcher';

const mocks = vi.hoisted(() => ({
  prompt: vi.fn(),
  capture: vi.fn(),
  upload: vi.fn(),
  find: vi.fn(),
  config: vi.fn(),
}));
vi.mock('vscode', async () => {
  const host = await import('./test-vscode');
  const disposable = { dispose() {} };
  const watcher = () => ({
    ...disposable,
    onDidChange: () => disposable,
    onDidCreate: () => disposable,
    onDidDelete: () => disposable,
  });
  return {
    ...host,
    workspace: {
      ...host.workspace,
      workspaceFolders: [{ name: 'project', uri: { scheme: 'file', fsPath: '/projects/app' } }],
      textDocuments: [],
      createFileSystemWatcher: watcher,
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
vi.mock('@gridstator/discovery', () => ({
  discoverDockerfiles: async () => [],
  discoverPackageScripts: async () => [],
  discoverWorkflows: async () => [],
  scriptCommand: vi.fn(),
}));

const NAME = 'GRIDSTATOR_LAUNCH_TEST_SECRET';
const VALUE = 'private-launch-token';
const config = ProjectConfigSchema.parse({
  version: 1,
  tasks: { check: { command: 'npm test', secretEnv: [NAME] } },
});
const launchers: Launcher[] = [];

beforeEach(() => {
  vi.resetAllMocks();
  vi.stubEnv(NAME, undefined);
  mocks.find.mockResolvedValue({
    root: '/projects/app',
    workdir: '.',
    configPath: '/projects/app/gridstator.yml',
  });
  mocks.config.mockResolvedValue(config);
  mocks.capture.mockResolvedValue({
    id: 'sha256:snapshot',
    stats: { files: 1, totalBytes: 10 },
    warnings: [],
  });
  mocks.prompt.mockResolvedValue(VALUE);
});
afterEach(() => {
  for (const launcher of launchers.splice(0)) launcher.dispose();
  vi.unstubAllEnvs();
});

async function setup(mode: 'node' | 'native') {
  const job: Job = {
    id: 'submitted-job',
    name: 'check',
    kind: 'command',
    state: 'queued',
    stateReason: null,
    priority: 0,
    owner: 'test',
    spec: JobSpecSchema.parse({
      kind: 'command',
      command: ['npm', 'test'],
      snapshot: `sha256:${'a'.repeat(64)}`,
      secretEnv: { [NAME]: '***' },
    }),
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
  const client = { submitJob: vi.fn().mockResolvedValue(job) };
  const state = {
    requireClient: () => client,
    connection: { clusterId: 'test-cluster' },
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
            workflows: [],
            packages: [],
            dockerfiles: [],
          }
        : job,
    ),
  };
  const storage = { get: vi.fn().mockResolvedValue(undefined), store: vi.fn(), delete: vi.fn() };
  const log = { appendLine: vi.fn() };
  const launcher = new Launcher(
    state as unknown as ClusterState,
    log as unknown as vscode.OutputChannel,
    mode === 'native' ? (native as unknown as NativeCli) : undefined,
    new JobSecrets(storage),
  );
  launchers.push(launcher);
  await launcher.refresh();
  expect(launcher.items).toContainEqual(expect.objectContaining({ id: 'task:project:check' }));
  native.execute.mockClear();
  return { launcher, native, client, storage, log };
}

describe.each(['node', 'native'] as const)('%s launcher job secrets', (mode) => {
  it('resolves before submission and keeps secret values out of request, origin and logs', async () => {
    const { launcher, native, client, storage, log } = await setup(mode);
    const request = { itemId: 'task:project:check', options: { env: { PUBLIC: 'visible' } } };
    await launcher.launch(request);
    const spec = mode === 'native' ? native.execute.mock.calls[0]?.[0] : undefined;
    if (mode === 'native') {
      expect(spec).toEqual(
        expect.objectContaining({
          operation: 'submit',
          spec: expect.objectContaining({
            secretEnv: { [NAME]: VALUE },
            env: { PUBLIC: 'visible' },
          }),
        }),
      );
      expect(mocks.capture).not.toHaveBeenCalled();
    } else {
      expect(client.submitJob).toHaveBeenCalledWith(
        expect.objectContaining({ secretEnv: { [NAME]: VALUE }, env: { PUBLIC: 'visible' } }),
      );
      expect(mocks.capture).toHaveBeenCalledTimes(1);
      expect(mocks.upload).toHaveBeenCalledTimes(1);
    }
    expect(storage.store).toHaveBeenCalledTimes(1);
    expect(JSON.stringify(request)).not.toContain(VALUE);
    expect(JSON.stringify([...launcher.origins.values()])).not.toContain(VALUE);
    expect(JSON.stringify(log.appendLine.mock.calls)).not.toContain(VALUE);
    expect(launcher.origins.get('submitted-job')?.request).toEqual(request);
  });

  it.each(['cancel', 'storage failure'] as const)(
    'stops before snapshot or submission on %s',
    async (failure) => {
      const { launcher, native, client, storage } = await setup(mode);
      if (failure === 'cancel') mocks.prompt.mockResolvedValue(undefined);
      else storage.get.mockRejectedValue(new Error('keychain unavailable'));
      const launch = launcher.launch({ itemId: 'task:project:check', options: {} });
      if (failure === 'cancel') await expect(launch).rejects.toBeInstanceOf(SecretInputCancelled);
      else await expect(launch).rejects.toMatchObject({ code: 'CONFIG' });
      expect(mocks.capture).not.toHaveBeenCalled();
      expect(mocks.upload).not.toHaveBeenCalled();
      expect(client.submitJob).not.toHaveBeenCalled();
      expect(native.execute).not.toHaveBeenCalled();
      expect(launcher.origins.size).toBe(0);
    },
  );
});

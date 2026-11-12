import { mkdtemp, rm } from 'node:fs/promises';
import { hostname, tmpdir } from 'node:os';
import { join } from 'node:path';
import type * as vscode from 'vscode';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GridstatorClient, writeClientConfig } from '@gridstator/client';
import { initController, startController, type RunningController } from '@gridstator/controller';
import {
  AGENT_PROTOCOL_VERSION,
  ControllerMessageSchema,
  CreateTokenResponseSchema,
  type ControllerMessage,
  type NodeInfo,
} from '@gridstator/protocol';
import { decodeWsData, machineIdentity, type BuildIdentity } from '@gridstator/shared';
import { LocalController } from './local-controller';
import { DashboardSettings } from './settings';
import { ClusterState } from './state';
import { testHost, Uri } from './test-vscode';
import type { ToWebview } from './messages';

const RESTORE_KEY = 'controllerUpdate.pendingWorkerRestore';
const log: vscode.OutputChannel = {
  name: 'settings-test',
  append: () => undefined,
  appendLine: () => undefined,
  replace: () => undefined,
  clear: () => undefined,
  show: () => undefined,
  hide: () => undefined,
  dispose: () => undefined,
};
const OLD: BuildIdentity = {
  id: '0.1.0-old',
  version: '0.1.0',
  sha256: null,
  packageDir: null,
  install: null,
};
const NEXT: BuildIdentity = {
  id: '0.2.0-next',
  version: '0.2.0',
  sha256: null,
  packageDir: null,
  install: null,
};
const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Shared persistent extension storage, as observed by independently opened SSH windows. */
class SharedStorage {
  private readonly values = new Map<string, unknown>();
  get<T>(key: string, fallback: T): T {
    return (this.values.get(key) ?? fallback) as T;
  }
  async update(key: string, value: unknown): Promise<void> {
    this.values.set(key, value);
  }
}

let directory: string;
let dataDir: string;
let configPath: string;
let controller: RunningController | undefined;
let admin: GridstatorClient;
let storage: SharedStorage;
let worker: ScriptedWorker;
const windows: Array<{ state: ClusterState; settings: DashboardSettings }> = [];
const extraControllers: RunningController[] = [];
let restarting: Promise<void> | undefined;

class ScriptedWorker {
  private socket: ReturnType<GridstatorClient['openWebSocket']> | undefined;
  private client: GridstatorClient | undefined;
  private readonly messages: ControllerMessage[] = [];
  readonly nodeId: string;

  constructor(private readonly info: NodeInfo) {
    const registered = controller!.registerLocalAgent('controller-worker', info);
    this.nodeId = registered.nodeId;
    this.credential = registered.credential;
  }
  private readonly credential: string;

  async connect(): Promise<void> {
    this.socket?.terminate();
    this.client?.close();
    this.client = new GridstatorClient({
      url: controller!.url,
      ca: controller!.caPem,
      token: this.credential,
    });
    const socket = this.client.openWebSocket('/v1/agent/ws');
    this.socket = socket;
    socket.on('message', (data) =>
      this.messages.push(ControllerMessageSchema.parse(JSON.parse(decodeWsData(data)))),
    );
    await new Promise<void>((resolve, reject) => {
      socket.once('open', resolve);
      socket.once('error', reject);
    });
    socket.send(
      JSON.stringify({
        type: 'hello',
        protocolVersion: AGENT_PROTOCOL_VERSION,
        info: this.info,
        activeJobs: [],
        results: [],
      }),
    );
    await expect.poll(async () => (await admin.getNode(this.nodeId)).status).toBe('online');
  }

  async startJob(): Promise<void> {
    const job = await admin.submitJob({
      kind: 'command',
      image: 'alpine',
      command: ['sleep', '100'],
      resources: { cpus: 1, memoryMb: 128 },
      target: { node: this.nodeId },
    });
    await expect
      .poll(() =>
        this.messages.some((message) => message.type === 'job.assign' && message.jobId === job.id),
      )
      .toBe(true);
    const assignment = this.messages.find(
      (message): message is Extract<ControllerMessage, { type: 'job.assign' }> =>
        message.type === 'job.assign' && message.jobId === job.id,
    )!;
    this.socket!.send(
      JSON.stringify({
        type: 'job.state',
        jobId: job.id,
        assignmentId: assignment.assignmentId,
        phase: 'running',
        ts: Date.now(),
      }),
    );
    await expect.poll(async () => (await admin.getJob(job.id)).state).toBe('running');
  }

  close(): void {
    this.socket?.terminate();
    this.client?.close();
  }
}

async function openWindow(
  post: (message: ToWebview) => void = () => undefined,
): Promise<{ state: ClusterState; settings: DashboardSettings }> {
  const state = new ClusterState(
    () => configPath,
    () => 100,
    log,
  );
  const context = {
    extension: { id: 'gridstator.gridstator', packageJSON: { version: '0.1.0' } },
    extensionUri: Uri.file(directory),
    globalState: storage,
  } as unknown as vscode.ExtensionContext;
  const local = new LocalController(context, () => configPath, log);
  vi.spyOn(local, 'refresh').mockResolvedValue(null);
  const settings = new DashboardSettings(context, state, local, post, () => undefined);
  const opened = { state, settings };
  windows.push(opened);
  await state.connect();
  return opened;
}

beforeEach(async () => {
  testHost.reset();
  directory = await mkdtemp(join(tmpdir(), 'gridstator-settings-'));
  dataDir = join(directory, 'controller');
  configPath = join(directory, 'client.json');
  testHost.configure('configPath', configPath);
  const init = await initController({ dataDir, host: '127.0.0.1', port: 0 });
  controller = await startController({ dataDir, host: '127.0.0.1', port: 0, build: OLD });
  await writeClientConfig(
    {
      version: 1,
      url: controller.url,
      caPem: init.caPem,
      caFingerprint: init.fingerprint,
      token: init.adminToken,
    },
    configPath,
  );
  admin = new GridstatorClient({ url: controller.url, ca: init.caPem, token: init.adminToken });
  storage = new SharedStorage();
  worker = new ScriptedWorker({
    hostname: hostname(),
    machineId: await machineIdentity(),
    platform: 'linux',
    osRelease: 'test',
    arch: 'x64',
    cpus: 4,
    memoryTotalMb: 4096,
    agentVersion: '0.1.0',
    docker: { available: true },
    buildx: { available: false },
    act: { available: false },
    executors: ['command'],
  });
  await worker.connect();
});

afterEach(async () => {
  windows.splice(0).forEach((window) => {
    window.settings.dispose();
    window.state.dispose();
  });
  await restarting;
  restarting = undefined;
  worker.close();
  admin.close();
  await controller?.close();
  controller = undefined;
  for (const extra of extraControllers.splice(0)) await extra.close();
  vi.restoreAllMocks();
  await rm(directory, { recursive: true, force: true });
});

describe('worker join command generation', () => {
  const url = 'https://192.168.1.77:7300';

  it('issues a one-use token valid for 24 hours and delivers its command only transiently', async () => {
    const posted: ToWebview[] = [];
    const first = await openWindow((message) => posted.push(message));
    const createToken = vi.spyOn(first.state.requireClient(), 'createToken');
    const persist = vi.spyOn(storage, 'update');
    const startedAt = Date.now();
    await first.settings.handle({
      type: 'createWorkerJoinCommand',
      name: '  windows-worker  ',
      url: `${url}/`,
    });
    const created = CreateTokenResponseSchema.parse(await createToken.mock.results[0]!.value);
    const message = posted.find((message) => message.type === 'workerJoinCommandCreated');
    expect(message).toMatchObject({
      type: 'workerJoinCommandCreated',
      result: {
        name: 'windows-worker',
        clusterId: controller!.clusterId,
        expiresAt: created.info.expiresAt,
      },
    });
    if (message?.type !== 'workerJoinCommandCreated') throw new Error('No join command delivered');
    expect(message.result.command).toContain(`gridstator agent join '${url}'`);
    expect(message.result.command).toContain(first.state.connection.caFingerprint);
    expect(message.result.command).toContain(created.token);
    expect(created.info.expiresAt).toBeGreaterThanOrEqual(startedAt + 24 * 3600 * 1000);
    expect(created.info.expiresAt).toBeLessThanOrEqual(Date.now() + 24 * 3600 * 1000);
    expect(await admin.listTokens()).toContainEqual(
      expect.objectContaining({ name: 'windows-worker', kind: 'join', usesRemaining: 1 }),
    );
    expect(JSON.stringify(first.settings.snapshot())).not.toContain(created.token);
    expect(JSON.stringify(persist.mock.calls)).not.toContain(created.token);

    const nodeInfo = (await admin.getNode(worker.nodeId)).info!;
    await expect(
      admin.registerAgent(created.token, { name: 'windows-worker', info: nodeInfo }),
    ).resolves.toMatchObject({ name: 'windows-worker' });
    await expect(
      admin.registerAgent(created.token, { name: 'second-worker', info: nodeInfo }),
    ).rejects.toMatchObject({ code: 'UNAUTHORIZED' });
  });

  it('requires an admin login before issuing a token', async () => {
    const posted: ToWebview[] = [];
    const first = await openWindow((message) => posted.push(message));
    const createToken = vi.spyOn(first.state.requireClient(), 'createToken');
    first.state.connection.role = 'operator';
    await expect(
      first.settings.handle({ type: 'createWorkerJoinCommand', name: 'windows-worker', url }),
    ).rejects.toMatchObject({ code: 'FORBIDDEN' });
    expect(createToken).not.toHaveBeenCalled();
    expect(posted).toEqual([]);
  });

  it.each([
    'http://192.168.1.77:7300',
    'https://127.0.0.1:7300',
    'https://localhost:7300',
    'https://[::1]:7300',
    'https://0.0.0.0:7300',
    'https://192.168.1.77:7300/path',
    'https://192.168.1.77:7300?token=private',
  ])('rejects an unusable worker endpoint before issuing a token (%s)', async (endpoint) => {
    const first = await openWindow();
    const createToken = vi.spyOn(first.state.requireClient(), 'createToken');
    await expect(
      first.settings.handle({
        type: 'createWorkerJoinCommand',
        name: 'windows-worker',
        url: endpoint,
      }),
    ).rejects.toMatchObject({ code: 'BAD_REQUEST' });
    expect(createToken).not.toHaveBeenCalled();
  });

  it('does not deliver a command after the selected controller changes during issuance', async () => {
    const posted: ToWebview[] = [];
    const first = await openWindow((message) => posted.push(message));
    const client = first.state.requireClient();
    const issue = client.createToken.bind(client);
    let release: () => void = () => undefined;
    const delayed = new Promise<void>((resolve) => {
      release = resolve;
    });
    let issued: () => void = () => undefined;
    const issuance = new Promise<void>((resolve) => {
      issued = resolve;
    });
    vi.spyOn(client, 'createToken').mockImplementationOnce(async (request) => {
      const result = await issue(request);
      issued();
      await delayed;
      return result;
    });
    const creating = first.settings.handle({
      type: 'createWorkerJoinCommand',
      name: 'windows-worker',
      url,
    });
    const outcome = expect(creating).rejects.toThrow(/selected controller changed/);
    await issuance;
    const otherDir = join(directory, 'other-controller');
    const init = await initController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    const other = await startController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    extraControllers.push(other);
    await writeClientConfig(
      {
        version: 1,
        url: other.url,
        caPem: init.caPem,
        caFingerprint: init.fingerprint,
        token: init.adminToken,
      },
      configPath,
    );
    await first.state.connect();
    release();
    await outcome;
    expect(first.state.connection.clusterId).toBe(other.clusterId);
    expect(posted).toEqual([]);
  });
});

describe('controller update lifecycle across SSH windows', () => {
  it('a second window leaves the first window’s active worker drain in place', async () => {
    const first = await openWindow();
    await worker.startJob();
    const install = vi.spyOn(GridstatorClient.prototype, 'installRelease');
    const updating = first.settings.handle({ type: 'installRelease', tag: 'v0.2.0' });
    await expect
      .poll(async () => (await admin.getNode(worker.nodeId)).settings.schedulingMode)
      .toBe('draining');
    await expect
      .poll(() =>
        testHost.progressMessages.some((message) =>
          message.startsWith('Waiting for 1 running job'),
        ),
      )
      .toBe(true);
    const second = await openWindow();
    await second.settings.refresh();
    expect((await admin.getNode(worker.nodeId)).settings.schedulingMode).toBe('draining');
    expect(storage.get<unknown[]>(RESTORE_KEY, [])).toHaveLength(1);
    expect(install).not.toHaveBeenCalled();
    await first.settings.handle({ type: 'cancelControllerInstall' });
    await updating;
    expect((await admin.getNode(worker.nodeId)).settings.schedulingMode).toBe('enabled');
  });

  it.each(['ready', 'expired'] as const)(
    'a new window restores a %s update’s worker without changing other clusters',
    async (status) => {
      await admin.updateNode(worker.nodeId, { schedulingMode: 'draining' });
      const foreign = { clusterId: 'another-cluster', nodeId: 'unrelated-worker', ready: true };
      await storage.update(RESTORE_KEY, [
        {
          clusterId: controller!.clusterId,
          nodeId: worker.nodeId,
          owner: 'previous-window',
          ready: status === 'ready',
          expiresAt: Date.now() + (status === 'ready' ? 60_000 : -1),
        },
        foreign,
      ]);
      await openWindow();
      await expect
        .poll(async () => (await admin.getNode(worker.nodeId)).settings.schedulingMode)
        .toBe('enabled');
      expect(storage.get(RESTORE_KEY, [])).toEqual([foreign]);
    },
  );

  it('changing the shared cluster while waiting cancels installation and restores the original worker', async () => {
    const first = await openWindow();
    await worker.startJob();
    const install = vi.spyOn(GridstatorClient.prototype, 'installRelease');
    const updating = first.settings.handle({ type: 'installRelease', tag: 'v0.2.0' });
    const outcome = expect(updating).rejects.toThrow(/selected cluster changed/);
    await expect
      .poll(() =>
        testHost.progressMessages.some((message) =>
          message.startsWith('Waiting for 1 running job'),
        ),
      )
      .toBe(true);
    const otherDir = join(directory, 'other-controller');
    const init = await initController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    const other = await startController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    extraControllers.push(other);
    await writeClientConfig(
      {
        version: 1,
        url: other.url,
        caPem: init.caPem,
        caFingerprint: init.fingerprint,
        token: init.adminToken,
      },
      configPath,
    );
    await first.state.connect();
    await outcome;
    expect(first.state.connection.clusterId).toBe(other.clusterId);
    expect(install).not.toHaveBeenCalled();
    expect((await admin.getNode(worker.nodeId)).settings.schedulingMode).toBe('enabled');
    expect(storage.get(RESTORE_KEY, [])).toEqual([]);
  });

  it('rejects a changed login before the connection watcher has adopted its new cluster', async () => {
    const first = await openWindow();
    const initialCluster = first.state.connection.clusterId;
    const otherDir = join(directory, 'other-controller');
    const init = await initController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    const other = await startController({ dataDir: otherDir, host: '127.0.0.1', port: 0 });
    extraControllers.push(other);
    await writeClientConfig(
      {
        version: 1,
        url: other.url,
        caPem: init.caPem,
        caFingerprint: init.fingerprint,
        token: init.adminToken,
      },
      configPath,
    );
    // Config-file delivery to an existing window is asynchronous. Its displayed cluster is
    // deliberately still the original one when the user confirms this update.
    expect(first.state.connection.clusterId).toBe(initialCluster);
    const install = vi.spyOn(GridstatorClient.prototype, 'installRelease').mockResolvedValue({
      action: 'none',
      from: other.build.ref.id,
      to: other.build.ref.id,
    });
    await expect(first.settings.handle({ type: 'installRelease', tag: 'v0.2.0' })).rejects.toThrow(
      /cluster|connection/i,
    );
    expect(install).not.toHaveBeenCalled();
    expect((await admin.getNode(worker.nodeId)).settings.schedulingMode).toBe('enabled');
  });

  it('keeps workers drained through a slow real controller restart, then restores scheduling', async () => {
    const first = await openWindow();
    let installBegan = false;
    vi.spyOn(GridstatorClient.prototype, 'installRelease').mockImplementationOnce(async () => {
      installBegan = true;
      const port = controller!.port;
      await controller!.close();
      controller = undefined;
      restarting = delay(1600).then(async () => {
        controller = await startController({ dataDir, host: '127.0.0.1', port, build: NEXT });
        await worker.connect();
      });
      return { action: 'restarting', from: OLD.id, to: NEXT.id };
    });
    const updating = first.settings.handle({ type: 'installRelease', tag: 'v0.2.0' });
    await expect.poll(() => installBegan).toBe(true);
    await delay(1100);
    expect(storage.get<unknown[]>(RESTORE_KEY, [])).toHaveLength(1);
    expect(controller).toBeUndefined();
    await updating;
    expect(first.state.connection.build?.id).toBe(NEXT.id);
    expect((await admin.getNode(worker.nodeId)).settings.schedulingMode).toBe('enabled');
    expect(storage.get(RESTORE_KEY, [])).toEqual([]);
  });
});

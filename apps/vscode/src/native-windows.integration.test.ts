/** Retained window models against a real Rust backend. This is not an editor/SSH-window test. */
import { execFile, spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { createWriteStream, readFileSync } from 'node:fs';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { promisify } from 'node:util';
import type * as vscode from 'vscode';
import type * as ClientModule from '@gridstator/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  ClientConfigSchema,
  GridstatorClient,
  watchClientConfig,
  writeClientConfig,
  type ClientConfig,
} from '@gridstator/client';
import {
  AGENT_PROTOCOL_VERSION,
  ControllerMessageSchema,
  type ControllerMessage,
  type NodeInfo,
} from '@gridstator/protocol';
import { decodeWsData } from '@gridstator/shared';
import { LocalController } from './local-controller';
import { NativeCli, effectiveClientConfigPath, nativeEnabled } from './native';
import { DashboardSettings } from './settings';
import { ClusterState } from './state';
import { testHost, Uri, window as testWindow } from './test-vscode';

// This mock is file-wide and hoisted. A future test outside the describe hooks cannot
// construct, retarget or use a client until an actual private CLI fixture is registered.
const networkFence = vi.hoisted(() => {
  let scope: { root: string; config: string } | undefined;
  let policy: () => void = () => {
    throw Error('Private native test policy is not initialized');
  };
  const allowed = new Map<string, string>();
  const refused = () =>
    new Error(
      'Native window test refused a controller outside its active private fixture allowlist',
    );
  const origin = (url: string) => {
    const value = new URL(url);
    if (
      value.protocol !== 'https:' ||
      value.hostname !== '127.0.0.1' ||
      !value.port ||
      value.username ||
      value.password ||
      value.pathname !== '/' ||
      value.search ||
      value.hash
    )
      throw refused();
    return value.origin;
  };
  const assertActive = () => {
    if (!scope) throw refused();
    policy();
  };
  const assertUrl = (url: string) => {
    assertActive();
    if (!allowed.has(origin(url))) throw refused();
  };
  return {
    setPolicy(value: () => void) {
      policy = value;
    },
    begin(root: string, config: string) {
      allowed.clear();
      scope = { root, config };
    },
    end() {
      allowed.clear();
      scope = undefined;
    },
    current: () => scope,
    assertActive,
    assertUrl,
    register(url: string, clusterId: string) {
      assertActive();
      if (!clusterId) throw refused();
      allowed.set(origin(url), clusterId);
    },
    unregister(url: string) {
      allowed.delete(origin(url));
    },
    assertEndpoint(url: string, path: string) {
      assertUrl(url);
      if (!path.startsWith('/') || new URL(url + path).origin !== origin(url)) throw refused();
    },
    assertCluster(url: string, clusterId: unknown) {
      assertUrl(url);
      if (clusterId !== allowed.get(origin(url))) throw refused();
    },
  };
});
vi.mock('@gridstator/client', async (importOriginal) => {
  const actual = await importOriginal<typeof ClientModule>();
  type RequestArgs = Parameters<InstanceType<typeof actual.GridstatorClient>['request']>;
  class PrivateFixtureClient extends actual.GridstatorClient {
    constructor(options: ConstructorParameters<typeof actual.GridstatorClient>[0]) {
      networkFence.assertUrl(options.url);
      super(options);
    }
    override get url(): string {
      const value = super.url;
      networkFence.assertUrl(value);
      return value;
    }
    override setControllerUrl(value: string): void {
      networkFence.assertUrl(value);
      super.setControllerUrl(value);
    }
    override async request<T>(...args: RequestArgs): Promise<T> {
      networkFence.assertEndpoint(this.url, args[1]);
      const result = (await super.request(...args)) as T;
      if (args[0] === 'GET' && args[1] === '/v1/cluster') {
        networkFence.assertCluster(this.url, (result as { clusterId?: unknown }).clusterId);
      }
      return result;
    }
    override openWebSocket(
      path: string,
    ): ReturnType<InstanceType<typeof actual.GridstatorClient>['openWebSocket']> {
      networkFence.assertEndpoint(this.url, path);
      return super.openWebSocket(path);
    }
  }
  return { ...actual, GridstatorClient: PrivateFixtureClient };
});
function assertPrivateScope(): void {
  const scope = networkFence.current();
  if (
    !scope ||
    !nativeEnabled() ||
    effectiveClientConfigPath() !== scope.config ||
    configPath !== scope.config ||
    resolve(directory) !== scope.root
  ) {
    throw Error('Native window test requires its explicit private native configuration');
  }
}
networkFence.setPolicy(assertPrivateScope);
function privateConfigPath(): string {
  networkFence.assertActive();
  return configPath;
}
function assertPrivatePath(path: string): void {
  networkFence.assertActive();
  const root = networkFence.current()!.root;
  if (resolve(path) !== root && !resolve(path).startsWith(root + sep))
    throw Error('Native window fixture path escaped its private root');
}
function assertSavedPrivateLogin(): void {
  assertPrivateScope();
  const saved = ClientConfigSchema.parse(JSON.parse(readFileSync(configPath, 'utf8')));
  networkFence.assertUrl(saved.url);
}
const actions = new Set<Promise<unknown>>();
function handleSettings(
  model: { settings: DashboardSettings },
  message: Parameters<DashboardSettings['handle']>[0],
): Promise<boolean> {
  const action = (async () => {
    assertSavedPrivateLogin();
    const saved = ClientConfigSchema.parse(JSON.parse(await readFile(configPath, 'utf8')));
    const probe = new GridstatorClient({ url: saved.url, ca: saved.caPem, token: saved.token });
    try {
      await probe.cluster();
    } finally {
      probe.close();
    }
    return model.settings.handle(message);
  })();
  actions.add(action);
  void action.then(
    () => actions.delete(action),
    () => actions.delete(action),
  );
  return action;
}

const selectedBinary = process.env.GRIDSTATOR_NATIVE_WINDOWS_E2E;
const packageRoot = process.env.GRIDSTATOR_NATIVE_WINDOWS_RELEASES;
const execute = promisify(execFile);
const RESTORE_KEY = 'controllerUpdate.pendingWorkerRestore';
const pause = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));
const log: vscode.OutputChannel = {
  name: 'native-models',
  append: () => undefined,
  appendLine: () => undefined,
  replace: () => undefined,
  clear: () => undefined,
  show: () => undefined,
  hide: () => undefined,
  dispose: () => undefined,
};
class SharedStorage {
  private readonly values = new Map<string, unknown>();
  get<T>(key: string, fallback: T): T {
    return (this.values.get(key) ?? fallback) as T;
  }
  async update(key: string, value: unknown): Promise<void> {
    this.values.set(key, value);
  }
}
interface NativeManifest {
  schemaVersion: number;
  buildId: string;
  version: string;
  sourceRevision: string;
  protocol: { api: number; agent: number; update: number };
  artifacts: Record<
    string,
    {
      archive: { file: string; size: number; sha256: string };
      executable: { file: string; size: number; sha256: string };
    }
  >;
}
async function freePort(): Promise<number> {
  const server = createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw Error('private port');
  await new Promise<void>((done, reject) =>
    server.close((error) => (error ? reject(error) : done())),
  );
  return address.port;
}
async function canonicalManifest(manifest: NativeManifest): Promise<string> {
  const artifacts = Object.fromEntries(
    Object.keys(manifest.artifacts)
      .sort()
      .map((target) => {
        const value = manifest.artifacts[target]!;
        const digest = (item: typeof value.archive) => ({
          file: item.file,
          size: item.size,
          sha256: item.sha256,
        });
        return [target, { archive: digest(value.archive), executable: digest(value.executable) }];
      }),
  );
  return (
    JSON.stringify(
      {
        schemaVersion: manifest.schemaVersion,
        buildId: manifest.buildId,
        version: manifest.version,
        sourceRevision: manifest.sourceRevision,
        protocol: {
          api: manifest.protocol.api,
          agent: manifest.protocol.agent,
          update: manifest.protocol.update,
        },
        artifacts,
      },
      null,
      2,
    ) + '\n'
  );
}
/** Test preparation only. Real native release verify and supervisor independently verify every
 * executable/manifest byte; no build identity is invented or substituted into backend responses. */
async function prepareManaged(
  root: string,
  environment: NodeJS.ProcessEnv,
): Promise<{ supervisor: string; install: string; manifest: NativeManifest }> {
  assertPrivatePath(root);
  if (!packageRoot || !selectedBinary)
    throw Error('native window suite requires actual release fixture packages');
  const source = join(packageRoot, 'rehearsal-baseline');
  const manifestPath = join(source, 'gridstator-release.json');
  const bytes = await readFile(manifestPath);
  const manifest = JSON.parse(bytes.toString()) as NativeManifest;
  const payload = join(packageRoot, 'rehearsal-baseline-gridstator');
  const target = `linux/${process.arch === 'arm64' ? 'arm64' : 'amd64'}`;
  const archive = manifest.artifacts[target]?.archive.file;
  if (!archive) throw Error('host release artifact missing');
  await execute(
    payload,
    [
      'release',
      'verify',
      '--manifest',
      manifestPath,
      '--manifest-sha256',
      createHash('sha256').update(bytes).digest('hex'),
      '--archive',
      join(source, archive),
      '--executable',
      payload,
      '--target',
      target,
    ],
    { env: environment },
  );
  const install = join(root, 'managed');
  const version = join(install, 'versions', manifest.buildId);
  await mkdir(version, { recursive: true, mode: 0o700 });
  const executable = join(version, 'gridstator');
  await copyFile(payload, executable);
  await chmod(executable, 0o555);
  const canonical = await canonicalManifest(manifest);
  expect(canonical).toBe(bytes.toString('utf8')); // Frozen native producer's pretty JSON plus LF.
  await writeFile(join(version, 'release.json'), canonical, { mode: 0o600 });
  await writeFile(
    join(version, 'installed.json'),
    JSON.stringify({
      buildId: manifest.buildId,
      target,
      executable,
      manifestSha256: createHash('sha256').update(canonical).digest('hex'),
    }),
    { mode: 0o600 },
  );
  await writeFile(
    join(install, 'state.json'),
    JSON.stringify({
      schemaVersion: 1,
      current: manifest.buildId,
      previous: null,
      pending: null,
      lastRollback: null,
    }),
    { mode: 0o600 },
  );
  const supervisor = join(install, 'supervisor');
  await copyFile(payload, supervisor);
  await chmod(supervisor, 0o555);
  return { supervisor, install, manifest };
}
class RustController {
  readonly data: string;
  readonly configPath: string;
  readonly environment: NodeJS.ProcessEnv;
  config!: ClientConfig;
  admin!: GridstatorClient;
  child!: ChildProcess;
  clusterId = '';
  install: string | undefined;
  private output: ReturnType<typeof createWriteStream> | undefined;
  constructor(readonly root: string) {
    this.data = join(root, 'controller');
    this.configPath = join(root, 'admin.json');
    this.environment = {
      ...process.env,
      HOME: join(root, 'home'),
      XDG_CONFIG_HOME: join(root, 'home', '.config'),
      GRIDSTATOR_CONFIG: this.configPath,
      GRIDSTATOR_CONTROLLER_DATA: this.data,
      GRIDSTATOR_AGENT_DATA: join(root, 'agent'),
      GRIDSTATOR_URL: undefined,
      GRIDSTATOR_TOKEN: undefined,
      GRIDSTATOR_CA_FILE: undefined,
      GRIDSTATOR_SUPERVISED: undefined,
      GRIDSTATOR_INSTALL_ROOT: undefined,
      GRIDSTATOR_INSTALL_VERSION: undefined,
      GRIDSTATOR_DOCKER: '/nonexistent/native-window-model-docker',
      GRIDSTATOR_ACT: '/nonexistent/native-window-model-act',
    };
  }
  async start(managed = false): Promise<void> {
    networkFence.assertActive();
    assertPrivatePath(this.root);
    assertPrivatePath(this.configPath);
    assertPrivatePath(this.data);
    if (!selectedBinary) throw Error('GRIDSTATOR_NATIVE_WINDOWS_E2E is required');
    await mkdir(this.root, { recursive: true, mode: 0o700 });
    await mkdir(this.environment.HOME!, { recursive: true, mode: 0o700 });
    await execute(
      selectedBinary,
      [
        'controller',
        'init',
        '--host',
        '127.0.0.1',
        '--port',
        String(await freePort()),
        '--data-dir',
        this.data,
      ],
      { env: this.environment },
    );
    this.config = ClientConfigSchema.parse(JSON.parse(await readFile(this.configPath, 'utf8')));
    const initialized = JSON.parse(await readFile(join(this.data, 'controller.json'), 'utf8')) as {
      clusterId: string;
    };
    this.clusterId = initialized.clusterId;
    networkFence.register(this.config.url, this.clusterId);
    this.output = createWriteStream(join(this.root, 'native-controller.log'), { mode: 0o600 });
    await once(this.output, 'open');
    let binary = selectedBinary;
    let args = ['controller', 'start', '--data-dir', this.data];
    if (managed) {
      const staged = await prepareManaged(this.root, this.environment);
      binary = staged.supervisor;
      this.install = staged.install;
      args = [
        'supervise',
        '--install-root',
        staged.install,
        '--',
        'controller',
        'start',
        '--data-dir',
        this.data,
      ];
    }
    this.child = spawn(binary, args, {
      env: this.environment,
      stdio: ['ignore', this.output, this.output],
    });
    this.admin = new GridstatorClient({
      url: this.config.url,
      ca: this.config.caPem,
      token: this.config.token,
    });
    await expect
      .poll(
        () =>
          this.admin
            .health()
            .then(() => true)
            .catch(() => false),
        { timeout: 15_000 },
      )
      .toBe(true)
      .catch(async (error: unknown) => {
        const detail = await readFile(join(this.root, 'native-controller.log'), 'utf8').catch(
          () => 'log unavailable',
        );
        throw new Error(`Private native controller failed startup: ${detail}`, { cause: error });
      });
    const cluster = await this.admin.cluster();
    expect(cluster.clusterId).toBe(this.clusterId);
    expect(cluster.build?.runtime).toBe('rust');
    await this.admin.request('PATCH', '/v1/settings', {
      json: { autoUpdate: 'off', toolchain: { act: 'off' } },
    });
  }
  async close(): Promise<void> {
    this.admin?.close();
    if (this.child && this.child.exitCode === null && this.child.signalCode === null) {
      this.child.kill('SIGCONT');
      this.child.kill('SIGTERM');
      const ended = once(this.child, 'exit');
      const timer = setTimeout(() => this.child.kill('SIGKILL'), 15_000);
      try {
        await ended;
      } finally {
        clearTimeout(timer);
      }
    }
    this.output?.end();
    if (this.config) networkFence.unregister(this.config.url);
  }
  async pauseLauncher(): Promise<void> {
    if (!this.install || !this.child.pid) throw Error('managed native launcher required');
    // The lock prevents stopping the launcher inside its activation critical section.
    const script = `const cp=require('node:child_process');const pid=Number(process.argv[1]);process.kill(pid,'SIGSTOP');for(let n=0;n<100;n++){if(cp.execFileSync('ps',['-o','stat=','-p',String(pid)],{encoding:'utf8'}).trim().startsWith('T'))process.exit(0);}throw Error('launcher stop not observed');`;
    await execute('flock', [
      join(this.install, 'install.lock'),
      process.execPath,
      '-e',
      script,
      String(this.child.pid),
    ]);
  }
  resumeLauncher(): void {
    this.child.kill('SIGCONT');
  }
}
/** Same protocol-peer model as settings.test.ts; it simulates assignments, not Rust execution. */
class ProtocolWorker {
  private client: GridstatorClient | undefined;
  private socket: ReturnType<GridstatorClient['openWebSocket']> | undefined;
  private readonly messages: ControllerMessage[] = [];
  private credential = '';
  nodeId = '';
  constructor(private readonly server: RustController) {}
  private info!: NodeInfo;
  async register(): Promise<void> {
    networkFence.assertActive();
    const cluster = await this.server.admin.cluster();
    this.info = {
      hostname: cluster.controller!.hostname,
      machineId: cluster.controller!.machineId,
      platform: 'linux',
      osRelease: 'protocol-model-peer',
      arch: process.arch === 'arm64' ? 'arm64' : 'x64',
      cpus: 4,
      memoryTotalMb: 4096,
      agentVersion: '2.0.0-model-peer',
      docker: { available: true },
      buildx: { available: false },
      act: { available: false },
      executors: ['command'],
    };
    const token = await this.server.admin.createToken({
      kind: 'join',
      name: 'native-model-peer',
      maxUses: 1,
    });
    const registered = await this.server.admin.registerAgent(token.token, {
      name: 'controller-worker',
      info: this.info,
    });
    this.nodeId = registered.nodeId;
    this.credential = registered.credential;
    await this.connect();
  }
  async connect(): Promise<void> {
    this.close();
    this.messages.length = 0;
    this.client = new GridstatorClient({
      url: this.server.config.url,
      ca: this.server.config.caPem,
      token: this.credential,
    });
    const socket = this.client.openWebSocket('/v1/agent/ws');
    this.socket = socket;
    socket.on('message', (data) =>
      this.messages.push(ControllerMessageSchema.parse(JSON.parse(decodeWsData(data)))),
    );
    await new Promise<void>((done, reject) => {
      socket.once('open', done);
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
    await expect
      .poll(async () => (await this.server.admin.getNode(this.nodeId)).status)
      .toBe('online')
      .catch(async (error: unknown) => {
        const status = (await this.server.admin.getNode(this.nodeId)).status;
        throw new Error(
          `Private protocol peer failed online: status=${status}, messages=${JSON.stringify(this.messages)}`,
          { cause: error },
        );
      });
  }
  async startJob(): Promise<void> {
    assertSavedPrivateLogin();
    const job = await this.server.admin.submitJob({
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
    await expect.poll(async () => (await this.server.admin.getJob(job.id)).state).toBe('running');
  }
  close(): void {
    this.socket?.terminate();
    this.client?.close();
    this.socket = undefined;
    this.client = undefined;
  }
}

let directory = '';
let configPath = '';
let storage: SharedStorage;
let server: RustController;
const controllers: RustController[] = [];
const models: Array<{ state: ClusterState; settings?: DashboardSettings; native?: NativeCli }> = [];
const disposers: Array<() => void> = [];
const peers: ProtocolWorker[] = [];
async function boot(name = 'primary', managed = false): Promise<RustController> {
  const current = new RustController(join(directory, name));
  controllers.push(current);
  await current.start(managed);
  return current;
}
function nativeFor(current: RustController): NativeCli {
  const native = new NativeCli({
    enabled: () => {
      networkFence.assertActive();
      return nativeEnabled();
    },
    trusted: () => true,
    path: () => {
      assertSavedPrivateLogin();
      return selectedBinary!;
    },
    environment: () => {
      assertPrivatePath(current.data);
      assertSavedPrivateLogin();
      return current.environment;
    },
    workspaceRoots: () => [],
  });
  return native;
}
async function share(current = server, token = current.config.token): Promise<void> {
  assertPrivatePath(configPath);
  networkFence.assertUrl(current.config.url);
  await writeClientConfig({ ...current.config, token }, configPath);
}
function context(): vscode.ExtensionContext {
  return {
    extension: {
      id: 'gridstator.gridstator',
      packageJSON: { version: '2.0.0-native-test' },
    },
    extensionUri: Uri.file(directory),
    globalState: storage,
  } as unknown as vscode.ExtensionContext;
}
async function openWindow(): Promise<{ state: ClusterState; settings: DashboardSettings }> {
  assertSavedPrivateLogin();
  const state = new ClusterState(privateConfigPath, () => 100, log);
  const native = nativeFor(server);
  const local = new LocalController(context(), privateConfigPath, log, native);
  const settings = new DashboardSettings(
    context(),
    state,
    local,
    () => undefined,
    () => undefined,
  );
  models.push({ state, settings, native });
  await state.connect();
  return { state, settings };
}
async function worker(): Promise<ProtocolWorker> {
  const peer = new ProtocolWorker(server);
  peers.push(peer);
  await peer.register();
  return peer;
}

it('native model transport fence rejects an existing-looking controller before any request or WebSocket', () => {
  expect(
    () => new GridstatorClient({ url: 'https://192.168.1.77:7300', ca: '', token: 'never-sent' }),
  ).toThrow(/private fixture allowlist/);
  expect(
    () => new GridstatorClient({ url: 'https://127.0.0.1:7300', ca: '', token: 'never-sent' }),
  ).toThrow(/private fixture allowlist/);
});

describe.skipIf(!selectedBinary)(
  'retained window component models with real native controller',
  () => {
    beforeEach(async () => {
      if (!selectedBinary || !packageRoot)
        throw Error(
          'Set GRIDSTATOR_NATIVE_WINDOWS_E2E and GRIDSTATOR_NATIVE_WINDOWS_RELEASES to real native fixtures',
        );
      testHost.reset();
      vi.stubEnv('GRIDSTATOR_URL', undefined);
      vi.stubEnv('GRIDSTATOR_TOKEN', undefined);
      vi.stubEnv('GRIDSTATOR_CA_FILE', undefined);
      directory = await mkdtemp(join(tmpdir(), 'gridstator-native-window-models-'));
      configPath = join(directory, 'shared-client.json');
      testHost.configure('nativeCli.enabled', true);
      testHost.configure('nativeCli.path', resolve(selectedBinary));
      testHost.configure('configPath', configPath);
      expect(nativeEnabled()).toBe(true);
      networkFence.begin(resolve(directory), resolve(configPath));
      assertPrivateScope();
      storage = new SharedStorage();
    });
    afterEach(async () => {
      disposers.splice(0).forEach((dispose) => dispose());
      for (const current of controllers) if (current.install) current.resumeLauncher();
      for (const model of models) model.settings?.dispose();
      await Promise.allSettled([...actions]);
      models.splice(0).forEach((model) => {
        model.state.dispose();
        model.native?.dispose();
      });
      peers.splice(0).forEach((peer) => peer.close());
      for (const current of controllers.splice(0).reverse()) await current.close();
      networkFence.end();
      vi.restoreAllMocks();
      vi.unstubAllEnvs();
      testHost.reset();
      if (directory) await rm(directory, { recursive: true, force: true });
    });

    it('rejects unregistered controller URLs and invalidated HTTP/WebSocket clients before transport, then continues on the registered private cluster', async () => {
      server = await boot();
      await share();
      const before = await server.admin.cluster();
      expect(before.clusterId).toBe(server.clusterId);
      for (const url of ['https://192.168.1.77:7300', 'https://127.0.0.1:7300']) {
        expect(
          () => new GridstatorClient({ url, ca: server.config.caPem, token: 'never-sent' }),
        ).toThrow(/private fixture allowlist/);
      }
      const probe = new GridstatorClient({
        url: server.config.url,
        ca: server.config.caPem,
        token: server.config.token,
      });
      try {
        networkFence.unregister(server.config.url);
        await expect(probe.request('GET', '/v1/health')).rejects.toThrow(
          /private fixture allowlist/,
        );
        expect(() => probe.openWebSocket('/v1/events')).toThrow(/private fixture allowlist/);
      } finally {
        networkFence.register(server.config.url, server.clusterId);
      }
      expect((await probe.cluster()).clusterId).toBe(server.clusterId);
      const socket = probe.openWebSocket('/v1/events');
      await once(socket, 'open');
      socket.close();
      await once(socket, 'close');
      probe.close();
      testHost.configure('configPath', '/existing-looking/shared-client.json');
      expect(assertPrivateScope).toThrow(/explicit private native configuration/);
      testHost.configure('configPath', configPath);
      assertPrivateScope();
    });

    it('three model windows connect on first shared login and adopt rotated native credentials', async () => {
      server = await boot();
      for (let i = 0; i < 3; i++) {
        const state = new ClusterState(privateConfigPath, () => 100, log);
        models.push({ state });
        await state.connect();
        expect(state.connection.status).toBe('signedOut');
        disposers.push(
          watchClientConfig(
            configPath,
            () => {
              void state.connect();
            },
            25,
          ),
        );
      }
      await share();
      await expect
        .poll(() => models.map((model) => model.state.connection.status))
        .toEqual(['connected', 'connected', 'connected']);
      const rotated = await server.admin.createToken({
        kind: 'client',
        name: 'shared-after-rotation',
        role: 'admin',
      });
      await share(server, rotated.token);
      await expect
        .poll(() => models.map((model) => model.state.connection.user))
        .toEqual(['shared-after-rotation', 'shared-after-rotation', 'shared-after-rotation']);
      expect(
        models.every(
          ({ state }) =>
            state.connection.controller?.hostname &&
            state.connection.version &&
            state.connection.caFingerprint,
        ),
      ).toBe(true);
    });

    it('revoked native shared credentials stop reconnecting and expose explicit local restoration', async () => {
      server = await boot();
      const token = await server.admin.createToken({
        kind: 'client',
        name: 'window-admin',
        role: 'admin',
      });
      await share(server, token.token);
      let preparations = 0;
      const native = nativeFor(server);
      const local = new LocalController(context(), privateConfigPath, log, native);
      const state = new ClusterState(
        privateConfigPath,
        () => 100,
        log,
        async () => {
          preparations++;
          await local.prepare();
        },
        (failed) => ({ canRestoreLocalAccess: failed }),
      );
      models.push({ state, native });
      await state.connect();
      expect(state.connected).toBe(true);
      await server.admin.revokeToken(token.info.id);
      await state.connect();
      expect(state.connection.status).toBe('error');
      expect(state.connection.canRestoreLocalAccess).toBe(true);
      const count = preparations;
      await pause(1100);
      expect(preparations).toBe(count);
    });

    it('a second native model window leaves the first window’s active worker drain in place', async () => {
      server = await boot();
      await share();
      const peer = await worker();
      const first = await openWindow();
      await peer.startJob();
      const install = vi.spyOn(GridstatorClient.prototype, 'installRelease');
      const updating = handleSettings(first, { type: 'installRelease', tag: 'v2.0.0-rehearsal.2' });
      await expect
        .poll(async () => (await server.admin.getNode(peer.nodeId)).settings.schedulingMode)
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
      expect((await server.admin.getNode(peer.nodeId)).settings.schedulingMode).toBe('draining');
      expect(storage.get<unknown[]>(RESTORE_KEY, [])).toHaveLength(1);
      expect(install).not.toHaveBeenCalled();
      await handleSettings(first, { type: 'cancelControllerInstall' });
      await updating;
      expect((await server.admin.getNode(peer.nodeId)).settings.schedulingMode).toBe('enabled');
    });

    it.each(['ready', 'expired'] as const)(
      'a new native model window restores a %s worker lease without changing another cluster',
      async (status) => {
        server = await boot();
        await share();
        const peer = await worker();
        await server.admin.updateNode(peer.nodeId, { schedulingMode: 'draining' });
        const foreign = { clusterId: 'another-cluster', nodeId: 'unrelated-worker', ready: true };
        await storage.update(RESTORE_KEY, [
          {
            clusterId: server.clusterId,
            nodeId: peer.nodeId,
            owner: 'previous-window',
            ready: status === 'ready',
            expiresAt: Date.now() + (status === 'ready' ? 60_000 : -1),
          },
          foreign,
        ]);
        await openWindow();
        await expect
          .poll(async () => (await server.admin.getNode(peer.nodeId)).settings.schedulingMode)
          .toBe('enabled');
        expect(storage.get(RESTORE_KEY, [])).toEqual([foreign]);
      },
    );

    it('changing the shared native cluster while waiting cancels installation and restores the original worker', async () => {
      server = await boot();
      await share();
      const peer = await worker();
      const first = await openWindow();
      await peer.startJob();
      const install = vi.spyOn(GridstatorClient.prototype, 'installRelease');
      const updating = handleSettings(first, { type: 'installRelease', tag: 'v2.0.0-rehearsal.2' });
      const outcome = expect(updating).rejects.toThrow(/selected cluster changed/);
      await expect
        .poll(() =>
          testHost.progressMessages.some((message) =>
            message.startsWith('Waiting for 1 running job'),
          ),
        )
        .toBe(true);
      const other = await boot('other');
      await share(other);
      await first.state.connect();
      await outcome;
      expect(first.state.connection.clusterId).toBe(other.clusterId);
      expect(install).not.toHaveBeenCalled();
      expect((await server.admin.getNode(peer.nodeId)).settings.schedulingMode).toBe('enabled');
      expect(storage.get(RESTORE_KEY, [])).toEqual([]);
    });

    it('rejects a changed native login before its window watcher adopts the new cluster', async () => {
      server = await boot();
      await share();
      const peer = await worker();
      const first = await openWindow();
      const original = first.state.connection.clusterId;
      const other = await boot('other');
      await share(other);
      expect(first.state.connection.clusterId).toBe(original);
      const install = vi.spyOn(GridstatorClient.prototype, 'installRelease');
      await expect(
        handleSettings(first, { type: 'installRelease', tag: 'v2.0.0-rehearsal.2' }),
      ).rejects.toThrow(/cluster|connection/i);
      expect(install).not.toHaveBeenCalled();
      expect((await server.admin.getNode(peer.nodeId)).settings.schedulingMode).toBe('enabled');
    });

    it('keeps model workers drained through a slow real native controller update then restores scheduling', async () => {
      if (!packageRoot) throw Error('actual native update packages required');
      server = await boot('managed', true);
      await share();
      const peer = await worker();
      const first = await openWindow();
      const before = await server.admin.cluster();
      const beforeRuntime = JSON.parse(
        await readFile(join(server.data, 'runtime.json'), 'utf8'),
      ) as {
        pid: number;
      };
      const manifest = JSON.parse(
        await readFile(join(packageRoot, 'rehearsal-confirmed/gridstator-release.json'), 'utf8'),
      ) as NativeManifest;
      expect(before.build?.id).not.toBe(manifest.buildId);
      const bundle = join(
        packageRoot,
        'rehearsal-confirmed',
        `gridstator-${manifest.version}-native-bundle.tar.gz`,
      );
      vi.spyOn(testWindow, 'showOpenDialog').mockResolvedValue([Uri.file(bundle)]);
      await server.pauseLauncher();
      const updating = handleSettings(first, { type: 'uploadControllerBuild' });
      let settled = false;
      void updating.then(
        () => {
          settled = true;
        },
        () => {
          settled = true;
        },
      );
      try {
        await expect
          .poll(
            () => testHost.progressMessages.includes('Waiting for the controller to reconnect…'),
            {
              timeout: 30_000,
            },
          )
          .toBe(true);
        await expect
          .poll(
            () =>
              server.admin
                .health()
                .then(() => false)
                .catch(() => true),
            { timeout: 30_000 },
          )
          .toBe(true);
        await pause(1100);
        expect(storage.get<unknown[]>(RESTORE_KEY, [])).toHaveLength(1);
        expect(settled).toBe(false);
        server.resumeLauncher();
        await expect
          .poll(
            () =>
              server.admin
                .health()
                .then(() => true)
                .catch(() => false),
            { timeout: 30_000 },
          )
          .toBe(true);
        await peer.connect();
        await updating;
        expect(first.state.connection.build?.id).toBe(manifest.buildId);
        expect(first.state.connection.build?.runtime).toBe('rust');
        expect((await server.admin.getNode(peer.nodeId)).settings.schedulingMode).toBe('enabled');
        expect(storage.get(RESTORE_KEY, [])).toEqual([]);
        const afterRuntime = JSON.parse(
          await readFile(join(server.data, 'runtime.json'), 'utf8'),
        ) as {
          pid: number;
        };
        expect(afterRuntime.pid).not.toBe(beforeRuntime.pid);
        const installed = JSON.parse(
          await readFile(join(server.install!, 'state.json'), 'utf8'),
        ) as {
          current: string;
          pending: unknown;
        };
        expect(installed.current).toBe(manifest.buildId);
        expect(installed.pending).toBeNull();
      } finally {
        server.resumeLauncher();
        await updating.catch(() => undefined);
      }
    }, 120_000);
  },
);

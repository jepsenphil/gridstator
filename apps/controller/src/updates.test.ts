import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, readFileSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { WebSocket } from 'ws';
import {
  decodeWsData,
  packageIdentity,
  silentLogger,
  type BuildIdentity,
} from '@gridstator/shared';
import {
  AGENT_PROTOCOL_VERSION,
  DEFAULT_CLUSTER_SETTINGS,
  ControllerMessageSchema,
  inUpdateWindow,
  type AgentBuild,
  type ControllerMessage,
  type Node,
  type NodeInfo,
  type ToolName,
  type ToolPin,
  type UpdatesView,
  toolArtifactLocation,
} from '@gridstator/protocol';
import { GridstatorClient, readBody } from '@gridstator/client';
import { initController, startController, type RunningController } from './controller.js';
import type { Fetcher } from './toolchain.js';
import { UpdateManager, type UpdateNode } from './updates.js';

const TIMING = {
  heartbeatTimeoutMs: 5_000,
  lostTimeoutMs: 5_000,
  cancelTimeoutMs: 1_000,
  assignTimeoutMs: 2_000,
  sweepIntervalMs: 50,
  scheduleIntervalMs: 100,
};

const UPDATE_TIMING = {
  confirmWithinSeconds: 60,
  updateTimeoutMs: 1_500,
  recheckMs: 300,
  updateOnlyFreshMs: 5_000,
};

const OLD = 'agent-old';

function agentBuild(id: string, extra: Partial<AgentBuild> = {}): AgentBuild {
  return {
    id,
    version: id.split('-')[0] ?? '0.1.0',
    sha256: null,
    managed: true,
    node: 'v24.0.0',
    lastRollback: null,
    ...extra,
  };
}

function info(build?: AgentBuild): NodeInfo {
  return {
    hostname: 'fake',
    platform: 'linux',
    osRelease: '6.0',
    arch: 'x64',
    cpus: 8,
    memoryTotalMb: 16_384,
    agentVersion: '0.1.0',
    docker: { available: true, version: '29.0.0' },
    buildx: { available: true },
    act: { available: false },
    executors: ['command'],
    ...(build ? { build } : {}),
  };
}

let dir: string;
let ctrl: RunningController;
let admin: GridstatorClient;
let adminToken: string;
let target: BuildIdentity;
const agents: FakeAgent[] = [];

async function waitFor<T>(
  fn: () => Promise<T | undefined | false> | T | undefined | false,
  what: string,
  timeoutMs = 5000,
): Promise<T> {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

class FakeAgent {
  readonly received: ControllerMessage[] = [];
  ws: WebSocket | undefined;
  readonly client: GridstatorClient;

  constructor(
    readonly name: string,
    readonly nodeId: string,
    credential: string,
  ) {
    this.client = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: credential });
  }

  async connect(build: AgentBuild | undefined): Promise<void> {
    this.ws?.terminate();
    this.received.length = 0;
    const ws = this.client.openWebSocket('/v1/agent/ws');
    this.ws = ws;
    ws.on('message', (d) =>
      this.received.push(ControllerMessageSchema.parse(JSON.parse(decodeWsData(d)))),
    );
    await new Promise<void>((resolve, reject) => {
      ws.once('open', () => resolve());
      ws.once('error', reject);
    });
    ws.send(
      JSON.stringify({
        type: 'hello',
        protocolVersion: AGENT_PROTOCOL_VERSION,
        info: info(build),
        activeJobs: [],
        results: [],
      }),
    );
    await this.next('welcome');
  }

  send(msg: unknown): void {
    this.ws?.send(JSON.stringify(msg));
  }

  has(type: ControllerMessage['type']): boolean {
    return this.received.some((m) => m.type === type);
  }

  async next<T extends ControllerMessage['type']>(
    type: T,
    timeoutMs = 5000,
  ): Promise<Extract<ControllerMessage, { type: T }>> {
    return waitFor(
      () => {
        const i = this.received.findIndex((m) => m.type === type);
        return i === -1
          ? undefined
          : (this.received.splice(i, 1)[0] as Extract<ControllerMessage, { type: T }>);
      },
      `${type} for ${this.name}`,
      timeoutMs,
    );
  }

  disconnect(): void {
    this.ws?.terminate();
    this.ws = undefined;
  }

  close(): void {
    this.disconnect();
    this.client.close();
  }
}

function addAgent(name: string): FakeAgent {
  const reg = ctrl.registerLocalAgent(name, info());
  const a = new FakeAgent(name, reg.nodeId, reg.credential);
  agents.push(a);
  return a;
}

async function node(name: string): Promise<Node> {
  return admin.getNode(name);
}

async function waitForUpdateState(name: string, state: string, timeoutMs = 5000): Promise<Node> {
  return waitFor(
    async () => {
      const n = await node(name);
      return n.update?.state === state ? n : undefined;
    },
    `${name} to be ${state}`,
    timeoutMs,
  );
}

async function waitForRollout(
  pred: (v: UpdatesView) => boolean,
  what: string,
  timeoutMs = 5000,
): Promise<UpdatesView> {
  return waitFor(
    async () => {
      const v = await admin.updates();
      return pred(v) ? v : undefined;
    },
    what,
    timeoutMs,
  );
}

/** Submits a job pinned to the agent's node and reports it running. */
async function runJobOn(a: FakeAgent): Promise<{ jobId: string; assignmentId: string }> {
  const job = await admin.submitJob({
    kind: 'command',
    image: 'alpine',
    command: ['sleep', '100'],
    resources: { cpus: 1, memoryMb: 256 },
    target: { node: a.name },
  });
  const assign = await a.next('job.assign');
  expect(assign.jobId).toBe(job.id);
  a.send({
    type: 'job.state',
    jobId: job.id,
    assignmentId: assign.assignmentId,
    phase: 'running',
    ts: Date.now(),
  });
  return { jobId: job.id, assignmentId: assign.assignmentId };
}

function finishJob(a: FakeAgent, j: { jobId: string; assignmentId: string }): void {
  a.send({
    type: 'job.result',
    result: {
      jobId: j.jobId,
      assignmentId: j.assignmentId,
      outcome: 'succeeded',
      exitCode: 0,
      error: null,
      oomKilled: false,
      finishedAt: Date.now(),
      logSeq: 0,
      outputs: null,
      artifacts: [],
    },
  });
}

/** What a real agent does on `update.check`: ask, download, verify. Returns the response. */
async function fetchUpdate(a: FakeAgent) {
  const res = await a.client.checkAgentUpdate(OLD, 'normal');
  expect(res.action).toBe('update');
  if (res.action !== 'update') throw new Error('no update');
  const body = await readBody(await a.client.downloadAgentUpdate(res.build.id), 64 * 1024 * 1024);
  expect(body.length).toBe(res.package.size);
  expect(createHash('sha256').update(body).digest('hex')).toBe(res.package.sha256);
  return res;
}

async function setSettings(patch: Record<string, unknown>): Promise<void> {
  await admin.request('PATCH', '/v1/settings', { json: patch });
}

async function fakePackage(): Promise<string> {
  const pkg = join(dir, 'pkg');
  await mkdir(join(pkg, 'dist'), { recursive: true });
  await writeFile(
    join(pkg, 'package.json'),
    JSON.stringify({ name: 'gridstator', version: '0.2.0' }),
  );
  await writeFile(join(pkg, 'dist', 'gridstator.mjs'), 'console.log("new build");\n');
  return pkg;
}

/** Small stand-ins for the act and Node.js archives, served by a fake fetcher. */
const files = new Map<string, string>();
let pins: Record<ToolName, ToolPin>;

async function archive(
  name: string,
  inner: string,
  body: string,
): Promise<{ path: string; sha256: string }> {
  const src = join(dir, 'tool-src', name);
  await mkdir(join(src, inner, '..'), { recursive: true });
  await writeFile(join(src, inner), body);
  await chmod(join(src, inner), 0o755);
  const path = join(dir, `${name}.tar.gz`);
  execFileSync('tar', ['-czf', path, '-C', src, inner.split('/')[0]!]);
  return { path, sha256: createHash('sha256').update(readFileSync(path)).digest('hex') };
}

async function fakeTool(tool: ToolName, version: string): Promise<string> {
  const loc = toolArtifactLocation(tool, version, 'linux-x64');
  const a = await archive(`${tool}-${version}`, loc.bin, `#!/bin/sh\necho ${tool} ${version}\n`);
  files.set(loc.url, a.path);
  const sums = join(dir, `${tool}-${version}.sums`);
  await writeFile(sums, `${a.sha256}  ${loc.checksumName}\n`);
  files.set(loc.checksumsUrl, sums);
  return a.sha256;
}

const fetcher: Fetcher = async (url) => {
  const path = files.get(url);
  if (!path) throw new Error(`no network in tests (${url})`);
  return createReadStream(path);
};

async function makePins(): Promise<Record<ToolName, ToolPin>> {
  const pin = async (tool: ToolName, version: string): Promise<ToolPin> => {
    const sha256 = await fakeTool(tool, version);
    const loc = toolArtifactLocation(tool, version, 'linux-x64');
    const a = { url: loc.url, bin: loc.bin, sha256 };
    return { version, artifacts: { 'linux-x64': a, 'linux-arm64': a } };
  };
  return { act: await pin('act', '9.9.1'), node: await pin('node', '9.9.2') };
}

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-upd-'));
  files.clear();
  pins = await makePins();
  const data = join(dir, 'data');
  const init = await initController({ dataDir: data, host: '127.0.0.1', port: 0 });
  const pkg = await fakePackage();
  target = { ...(await packageIdentity(pkg)), packageDir: pkg, install: null };
  ctrl = await startController({
    dataDir: data,
    host: '127.0.0.1',
    port: 0,
    timing: TIMING,
    build: target,
    updateTiming: UPDATE_TIMING,
    toolchainFetcher: fetcher,
    toolchainPins: pins,
  });
  adminToken = init.adminToken;
  admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: adminToken });
  await waitForRollout((v) => v.controller.updatable, 'the update package');
  // Most tests are about the build; the toolchain has its own tests below.
  await setSettings({ toolchain: { act: 'off', node: 'off' } });
});

afterEach(async () => {
  for (const a of agents.splice(0)) a.close();
  admin.close();
  await ctrl.close();
  await rm(dir, { recursive: true, force: true });
});

describe('version status', () => {
  it('reports current, behind and unknown workers against the controller build', async () => {
    await setSettings({ autoUpdate: 'off' });
    const cur = addAgent('cur');
    const old = addAgent('old');
    const legacy = addAgent('legacy');
    const unmanaged = addAgent('unmanaged');
    addAgent('gone');
    await cur.connect(agentBuild(target.id));
    await old.connect(agentBuild(OLD));
    await legacy.connect(undefined);
    await unmanaged.connect(agentBuild(OLD, { managed: false }));

    const cluster = await admin.cluster();
    expect(cluster.build).toMatchObject({ id: target.id, updatable: true, reason: null });
    expect((await node('cur')).update).toMatchObject({
      state: 'current',
      build: target.id,
      autoUpdate: 'off',
      canUpdate: true,
    });
    expect((await node('old')).update).toMatchObject({
      state: 'behind',
      build: OLD,
      detail: expect.stringContaining('auto-update is off'),
    });
    expect((await node('legacy')).update).toMatchObject({
      state: 'behind',
      canUpdate: false,
      detail: expect.stringContaining('predates self-update'),
    });
    expect((await node('unmanaged')).update?.detail).toContain('not a managed install');
    expect((await node('gone')).update).toMatchObject({ state: 'unknown' });

    old.disconnect();
    expect((await waitForUpdateState('old', 'unknown')).update?.detail).toContain(
      `last reported ${OLD}`,
    );
  });

  it('cannot update workers from an unbundled controller', async () => {
    await ctrl.close();
    ctrl = await startController({
      dataDir: join(dir, 'data'),
      host: '127.0.0.1',
      port: 0,
      timing: TIMING,
      build: {
        id: '0.2.0-unbundled',
        version: '0.2.0',
        sha256: null,
        packageDir: null,
        install: null,
      },
    });
    admin.close();
    admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: adminToken });
    const v = await admin.updates();
    expect(v.controller).toMatchObject({ updatable: false });
    expect(v.controller.reason).toContain('unbundled');
    await expect(admin.startUpdate({})).rejects.toThrow(/cannot update/);
  });
});

describe('rolling updates', () => {
  beforeEach(async () => {
    await setSettings({ autoUpdate: 'off' });
  });

  it('waits without cancelling running jobs by default, even beyond the old drain deadline', () => {
    let now = Date.now();
    const worker: UpdateNode = {
      id: 'draining-worker',
      name: 'draining-worker',
      online: true,
      build: agentBuild(OLD),
      agentVersion: '0.1.0',
      autoUpdate: 'off',
      activeJobs: 1,
      platform: 'linux',
      arch: 'x64',
    };
    const cancelJobs = vi.fn();
    const sendUpdateCheck = vi.fn(() => true);
    const manager = new UpdateManager({
      store: ctrl.store,
      build: ctrl.build,
      settings: () => ({ ...DEFAULT_CLUSTER_SETTINGS, autoUpdate: 'off' }),
      logger: silentLogger(),
      now: () => now,
    });
    manager.attach({
      nodes: () => [worker],
      node: (id) => (id === worker.id ? worker : undefined),
      cancelJobs,
      sendUpdateCheck,
      changed: () => undefined,
      requestSchedule: () => undefined,
    });
    expect(manager.start({}, 'test').rollout?.drainTimeoutSeconds).toBeNull();
    now += 8 * 24 * 3600_000;
    manager.tick();
    expect(manager.view().rollout?.nodes[0]?.state).toBe('draining');
    expect(cancelJobs).not.toHaveBeenCalled();
    expect(sendUpdateCheck).not.toHaveBeenCalled();
    worker.activeJobs = 0;
    manager.tick();
    expect(manager.view().rollout?.nodes[0]?.state).toBe('updating');
    expect(sendUpdateCheck).toHaveBeenCalledOnce();
  });

  it('drains, updates one node at a time and finishes when nodes return on the new build', async () => {
    const a = addAgent('a');
    const b = addAgent('b');
    await a.connect(agentBuild(OLD));
    await b.connect(agentBuild(OLD));
    const job = await runJobOn(a);

    const started = await admin.startUpdate({ now: false, drainTimeoutSeconds: null });
    expect(started.rollout?.drainTimeoutSeconds).toBeNull();
    expect(started.rollout?.nodes.map((n) => n.name).sort()).toEqual(['a', 'b']);
    // a is draining (its job keeps running) and takes no new jobs.
    const draining = await waitForUpdateState('a', 'updating');
    expect(draining.update?.detail).toContain('draining');
    expect(await a.client.checkAgentUpdate(OLD, 'normal')).toMatchObject({ action: 'none' });
    await admin.submitJob({
      kind: 'command',
      image: 'alpine',
      command: ['true'],
      resources: { cpus: 1, memoryMb: 256 },
      target: { node: 'a' },
    });
    await new Promise((r) => setTimeout(r, 300));
    expect(a.has('job.assign')).toBe(false);
    expect(b.has('update.check')).toBe(false);

    finishJob(a, job);
    await a.next('update.check');
    const res = await fetchUpdate(a);
    expect(res).toMatchObject({ build: { id: target.id }, confirmWithinSeconds: 60 });

    // The agent restarts on the new build; then it is b's turn.
    a.disconnect();
    await a.connect(agentBuild(target.id));
    await b.next('update.check');
    await fetchUpdate(b);
    b.disconnect();
    await b.connect(agentBuild(target.id));

    const done = await waitForRollout((v) => v.rollout?.state === 'finished', 'rollout to finish');
    expect(done.rollout?.nodes.map((n) => n.state)).toEqual(['done', 'done']);
    expect((await node('a')).update?.state).toBe('current');
    expect((await node('b')).update?.state).toBe('current');
    // The held job runs on a once a is back.
    await a.next('job.assign');
  });

  it('cancels running jobs with --now', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    const job = await runJobOn(a);
    await admin.startUpdate({ nodes: ['a'], now: true });
    const cancel = await a.next('job.cancel');
    expect(cancel.jobId).toBe(job.jobId);
    a.send({
      type: 'job.result',
      result: {
        jobId: job.jobId,
        assignmentId: job.assignmentId,
        outcome: 'cancelled',
        exitCode: null,
        error: null,
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
        outputs: null,
        artifacts: [],
      },
    });
    await a.next('update.check');
    expect((await admin.getJob(job.jobId)).state).toBe('cancelled');
  });

  it('halts the rollout when a node reports a failed update, and does not retry it', async () => {
    const a = addAgent('a');
    const b = addAgent('b');
    await a.connect(agentBuild(OLD));
    await b.connect(agentBuild(OLD));
    await admin.startUpdate({ nodes: ['a', 'b'] });
    await a.next('update.check');
    await a.client.reportAgentUpdate(target.id, 'disk full');

    const halted = await waitForRollout((v) => v.rollout?.state === 'halted', 'halt');
    expect(halted.rollout?.haltReason).toBe('a: disk full');
    expect(halted.rollout?.nodes.map((n) => [n.name, n.state])).toEqual([
      ['a', 'failed'],
      ['b', 'pending'],
    ]);
    expect((await node('a')).update).toMatchObject({ state: 'failed', detail: 'disk full' });
    expect((await node('b')).update?.detail).toContain('rollout halted');
    await setSettings({ autoUpdate: 'follow' });
    await new Promise((r) => setTimeout(r, 300));
    expect(b.has('update.check')).toBe(false);

    // An operator retries explicitly.
    await admin.startUpdate({ nodes: ['a'] });
    await a.next('update.check');
  });

  it('fails a node the launcher rolled back', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    await admin.startUpdate({ nodes: ['a'] });
    await a.next('update.check');
    await fetchUpdate(a);
    a.disconnect();
    await a.connect(
      agentBuild(OLD, {
        lastRollback: { from: target.id, to: OLD, at: Date.now(), reason: 'crashed' },
      }),
    );
    const v = await waitForRollout((x) => x.rollout?.state === 'halted', 'halt');
    expect(v.rollout?.haltReason).toContain('the launcher rolled back');
    expect((await node('a')).update?.state).toBe('failed');
  });

  it('fails a node that does not come back in time', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    await admin.startUpdate({ nodes: ['a'] });
    await a.next('update.check');
    a.disconnect();
    const v = await waitForRollout((x) => x.rollout?.state === 'halted', 'timeout halt', 5000);
    expect(v.rollout?.haltReason).toContain('did not come back');
  });

  it('stops a rollout on request', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    await runJobOn(a);
    await admin.startUpdate({ nodes: ['a'] });
    await waitForUpdateState('a', 'updating');
    const v = await admin.cancelUpdate();
    expect(v.rollout).toMatchObject({
      state: 'halted',
      haltReason: expect.stringContaining('stopped by'),
    });
    expect((await node('a')).update?.state).toBe('behind');
  });
});

describe('auto-update', () => {
  it('follows the controller by default', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    await a.next('update.check');
    expect((await admin.updates()).rollout).toMatchObject({ startedBy: 'auto-update' });
  });

  it('honours node overrides and the update window', async () => {
    const now = new Date();
    const hhmm = (d: Date) =>
      `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
    const later = new Date(now.getTime() + 2 * 3600_000);
    const latest = new Date(now.getTime() + 3 * 3600_000);
    await expect(setSettings({ autoUpdate: 'window', updateWindow: null })).rejects.toThrow(
      /needs an update window/,
    );
    await setSettings({
      autoUpdate: 'window',
      updateWindow: { start: hhmm(later), end: hhmm(latest) },
    });
    const a = addAgent('a');
    await a.connect(agentBuild(OLD));
    await new Promise((r) => setTimeout(r, 300));
    expect(a.has('update.check')).toBe(false);
    expect((await node('a')).update?.detail).toContain('waiting for the update window');

    await admin.updateNode('a', { autoUpdate: 'follow' });
    await a.next('update.check');
  });

  it('updates agents that can only reach the update endpoint', async () => {
    const a = addAgent('a');
    // No agent channel: e.g. the agent speaks an older protocol.
    const first = await a.client.checkAgentUpdate(OLD, 'update-only');
    const res =
      first.action === 'update'
        ? first
        : await waitFor(async () => {
            const r = await a.client.checkAgentUpdate(OLD, 'update-only');
            return r.action === 'update' ? r : undefined;
          }, 'update for the update-only agent');
    expect(res.build.id).toBe(target.id);
    expect((await node('a')).update?.state).toBe('updating');
    await a.connect(agentBuild(target.id));
    await waitForUpdateState('a', 'current');
  });
});

describe('update window', () => {
  it('handles windows that wrap past midnight', () => {
    const at = (h: number, m = 0) => new Date(2026, 0, 1, h, m);
    expect(inUpdateWindow({ start: '02:00', end: '05:00' }, at(3))).toBe(true);
    expect(inUpdateWindow({ start: '02:00', end: '05:00' }, at(5))).toBe(false);
    expect(inUpdateWindow({ start: '22:00', end: '02:00' }, at(23, 30))).toBe(true);
    expect(inUpdateWindow({ start: '22:00', end: '02:00' }, at(1, 59))).toBe(true);
    expect(inUpdateWindow({ start: '22:00', end: '02:00' }, at(12))).toBe(false);
  });
});

describe('managed toolchain', () => {
  const SYSTEM = { act: '0.2.80', node: '24.15.0', managed: [] as ToolName[] };
  const MANAGED = { act: '9.9.1', node: '9.9.2', managed: ['act', 'node'] as ToolName[] };

  beforeEach(async () => {
    await setSettings({ autoUpdate: 'off', toolchain: { act: null, node: null } });
  });

  it('counts act and Node.js toward current and serves verified archives', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(target.id, { tools: SYSTEM }));
    const behind = await waitFor(async () => {
      const n = await node('a');
      // The two archives are prepared independently; one gap can appear before both are ready.
      return n.update?.detail?.includes('act 0.2.80 (system) → 9.9.1') &&
        n.update.detail.includes('node 24.15.0 (system) → 9.9.2')
        ? n
        : undefined;
    }, 'both toolchain gaps');
    expect(behind.update?.state).toBe('behind');
    expect(behind.update?.detail).toContain('act 0.2.80 (system) → 9.9.1');
    expect(behind.update?.detail).toContain('node 24.15.0 (system) → 9.9.2');
    const view = await admin.updates();
    expect(view.toolchain).toEqual([
      expect.objectContaining({ name: 'act', version: '9.9.1', source: 'pinned' }),
      expect.objectContaining({ name: 'node', version: '9.9.2', source: 'pinned' }),
    ]);

    const offered = await a.client.agentToolchain('linux', 'x64');
    const act = offered.tools.find((t) => t.name === 'act');
    expect(act).toMatchObject({ state: 'ready', version: '9.9.1', bin: 'act' });
    const body = await readBody(await a.client.downloadToolchain('act', '9.9.1', 'linux-x64'));
    expect(createHash('sha256').update(body).digest('hex')).toBe(act?.sha256);
    expect((await a.client.agentToolchain('darwin', 'arm64')).tools).toEqual([]);

    // A rollout step finishes once the agent runs the managed tools.
    await admin.startUpdate({ nodes: ['a'] });
    await a.next('update.check');
    a.disconnect();
    await a.connect(agentBuild(target.id, { tools: MANAGED }));
    await waitForUpdateState('a', 'current');
    expect((await admin.updates()).rollout?.state).toBe('finished');
  });

  it('follows a version set in the cluster settings, checked against upstream checksums', async () => {
    await fakeTool('act', '9.9.3');
    const a = addAgent('a');
    await a.connect(agentBuild(target.id, { tools: MANAGED }));
    await waitForUpdateState('a', 'current');
    await setSettings({ toolchain: { act: '9.9.3', node: null } });
    const n = await waitFor(async () => {
      const x = await node('a');
      return x.update?.detail?.includes('9.9.3') ? x : undefined;
    }, 'the override to apply');
    expect(n.update?.detail).toContain('act 9.9.1 → 9.9.3');
    const view = await admin.updates();
    expect(view.toolchain?.[0]).toMatchObject({
      name: 'act',
      version: '9.9.3',
      source: 'override',
    });

    await setSettings({ toolchain: { act: 'off', node: null } });
    await waitForUpdateState('a', 'current');
  });

  it('ignores the toolchain of agents that cannot update themselves', async () => {
    const a = addAgent('a');
    await a.connect(agentBuild(target.id, { managed: false, tools: SYSTEM }));
    expect((await node('a')).update?.state).toBe('current');
  });
});

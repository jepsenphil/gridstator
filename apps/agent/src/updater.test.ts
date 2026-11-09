import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, readFileSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  activateVersion,
  detectBuild,
  installFromPackageDir,
  installRoot,
  packageIdentity,
  readInstallState,
  type BuildIdentity,
} from '@gridstator/shared';
import type { DockerCli } from '@gridstator/executors';
import { toolArtifactLocation, type ToolName, type ToolPin } from '@gridstator/protocol';
import { GridstatorClient } from '@gridstator/client';
import { initController, startController, type RunningController } from '@gridstator/controller';
import { Agent, type AgentStatus } from './agent.js';
import type { AgentState } from './state.js';

/** Agents in these tests cannot run jobs; they only need to join and update. */
const NO_DOCKER = {
  probe: async () => ({ available: false, error: 'not used in this test' }),
  probeBuildx: async () => ({ available: false }),
} as unknown as DockerCli;

let dir: string;
/** Fake upstream: URL → local archive. */
const files = new Map<string, string>();

async function fakeTool(tool: ToolName, version: string): Promise<ToolPin> {
  const loc = toolArtifactLocation(tool, version, 'linux-x64');
  const src = join(dir, 'tool-src', tool);
  await mkdir(join(src, loc.bin, '..'), { recursive: true });
  await writeFile(join(src, loc.bin), `#!/bin/sh\necho "${tool} version ${version}"\n`);
  await chmod(join(src, loc.bin), 0o755);
  const path = join(dir, `${tool}.tar.gz`);
  execFileSync('tar', ['-czf', path, '-C', src, loc.bin.split('/')[0]!]);
  files.set(loc.url, path);
  const sha256 = createHash('sha256').update(readFileSync(path)).digest('hex');
  const a = { url: loc.url, bin: loc.bin, sha256 };
  return { version, artifacts: { 'linux-x64': a, 'linux-arm64': a } };
}

async function makePins(): Promise<Record<ToolName, ToolPin>> {
  return { act: await fakeTool('act', '9.9.1'), node: await fakeTool('node', '9.9.2') };
}
let ctrl: RunningController;
let admin: GridstatorClient;
const running: Agent[] = [];

async function waitFor<T>(
  fn: () => Promise<T | undefined | false> | T | undefined | false,
  what: string,
  timeoutMs = 10_000,
): Promise<T> {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

async function fakePackage(name: string, version: string): Promise<string> {
  const pkg = join(dir, 'pkgs', name);
  await mkdir(join(pkg, 'dist'), { recursive: true });
  await writeFile(join(pkg, 'package.json'), JSON.stringify({ name: 'gridstator', version }));
  await writeFile(join(pkg, 'dist', 'gridstator.mjs'), `// ${name}\n`);
  return pkg;
}

function managedBuild(root: string, id: string): Promise<BuildIdentity> {
  return detectBuild('0.0.0', undefined, {
    GRIDSTATOR_INSTALL_ROOT: root,
    GRIDSTATOR_INSTALL_VERSION: id,
  });
}

function startAgent(
  dataDir: string,
  state: AgentState,
  build: BuildIdentity,
  statuses: AgentStatus[],
): Promise<Agent> {
  const agent = new Agent({ dataDir, state, docker: NO_DOCKER, build, selfUpdate: true });
  agent.onStatus((s) => statuses.push(s));
  running.push(agent);
  return agent.start().then(() => agent);
}

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-agent-upd-'));
  const data = join(dir, 'controller');
  const init = await initController({ dataDir: data, host: '127.0.0.1', port: 0 });
  const pkg = await fakePackage('new', '0.2.0');
  files.clear();
  ctrl = await startController({
    dataDir: data,
    host: '127.0.0.1',
    port: 0,
    timing: { sweepIntervalMs: 50, scheduleIntervalMs: 100 },
    build: { ...(await packageIdentity(pkg)), packageDir: pkg, install: null },
    toolchainFetcher: async (url) => {
      const path = files.get(url);
      if (!path) throw new Error(`no network in tests (${url})`);
      return createReadStream(path);
    },
    toolchainPins: await makePins(),
  });
  admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: init.adminToken });
  await waitFor(async () => (await admin.updates()).controller.updatable, 'the update package');
  await admin.request('PATCH', '/v1/settings', {
    json: { toolchain: { act: 'off', node: 'off' } },
  });
});

afterEach(async () => {
  for (const a of running.splice(0)) await a.stop();
  admin.close();
  await ctrl.close();
  await rm(dir, { recursive: true, force: true });
});

describe('agent self-update', () => {
  it('installs the controller build, restarts, and confirms it after rejoining', async () => {
    const agentDir = join(dir, 'agent');
    const root = installRoot(agentDir);
    const oldId = await installFromPackageDir(root, await fakePackage('old', '0.1.0'));
    await activateVersion(root, oldId);
    const reg = ctrl.registerLocalAgent('w1', {
      hostname: 'w1',
      platform: 'linux',
      osRelease: '6',
      arch: 'x64',
      cpus: 2,
      memoryTotalMb: 2048,
      agentVersion: '0.1.0',
      docker: { available: false },
      buildx: { available: false },
      act: { available: false },
      executors: [],
    });
    const state: AgentState = {
      version: 1,
      controllerUrl: ctrl.url,
      caPem: ctrl.caPem,
      caFingerprint: ctrl.fingerprint,
      nodeId: reg.nodeId,
      name: 'w1',
      credential: reg.credential,
    };

    // The old version joins; auto-update (follow) sends it the controller's build.
    const statuses: AgentStatus[] = [];
    const first = await startAgent(agentDir, state, await managedBuild(root, oldId), statuses);
    await waitFor(() => statuses.includes('restarting'), 'the agent to ask for a restart');
    const target = ctrl.build.ref.id;
    const installed = await readInstallState(root);
    expect(installed).toMatchObject({
      current: target,
      previous: oldId,
      pending: { id: target, previous: oldId, attempts: 0 },
    });
    expect((await admin.getNode('w1')).update?.state).toBe('updating');
    await first.stop();

    // What the launcher starts next: the new version, which confirms itself once it joins.
    const after: AgentStatus[] = [];
    await startAgent(agentDir, state, await managedBuild(root, target), after);
    await waitFor(
      async () => (await readInstallState(root))?.pending === undefined,
      'the update to be confirmed',
    );
    const node = await waitFor(async () => {
      const n = await admin.getNode('w1');
      return n.update?.state === 'current' ? n : undefined;
    }, 'w1 to be current');
    expect(node.info?.build).toMatchObject({ id: target, managed: true });
    expect((await admin.updates()).rollout).toMatchObject({ state: 'finished' });
    expect(after).not.toContain('restarting');
  });

  it('reports a failed update instead of restarting when the install is not managed', async () => {
    const reg = ctrl.registerLocalAgent('w2', {
      hostname: 'w2',
      platform: 'linux',
      osRelease: '6',
      arch: 'x64',
      cpus: 2,
      memoryTotalMb: 2048,
      agentVersion: '0.1.0',
      docker: { available: false },
      buildx: { available: false },
      act: { available: false },
      executors: [],
    });
    const statuses: AgentStatus[] = [];
    // An unmanaged agent never becomes a candidate on its own; an operator forces it.
    await startAgent(
      join(dir, 'agent2'),
      {
        version: 1,
        controllerUrl: ctrl.url,
        caPem: ctrl.caPem,
        caFingerprint: ctrl.fingerprint,
        nodeId: reg.nodeId,
        name: 'w2',
        credential: reg.credential,
      },
      { id: '0.1.0-unbundled', version: '0.1.0', sha256: null, packageDir: null, install: null },
      statuses,
    );
    await waitFor(async () => (await admin.getNode('w2')).status === 'online', 'w2 online');
    const n = await admin.getNode('w2');
    expect(n.update).toMatchObject({ state: 'behind', canUpdate: false });
    expect(n.update?.detail).toContain('not a managed install');
    const v = await admin.startUpdate({ nodes: ['w2'] });
    expect(v.rollout?.nodes[0]).toMatchObject({
      state: 'skipped',
      detail: 'not a managed install',
    });
    expect(statuses).not.toContain('restarting');
  });

  const linux = process.platform === 'linux' && ['x64', 'arm64'].includes(process.arch);

  it.skipIf(!linux)(
    'installs the managed toolchain and reports when the launcher did not switch Node.js',
    async () => {
      const agentDir = join(dir, 'agent3');
      const root = installRoot(agentDir);
      const target = ctrl.build.ref.id;
      const id = await installFromPackageDir(root, join(dir, 'pkgs', 'new'));
      expect(id).toBe(target);
      await activateVersion(root, id);
      const reg = ctrl.registerLocalAgent('w3', {
        hostname: 'w3',
        platform: 'linux',
        osRelease: '6',
        arch: 'x64',
        cpus: 2,
        memoryTotalMb: 2048,
        agentVersion: '0.2.0',
        docker: { available: false },
        buildx: { available: false },
        act: { available: false },
        executors: [],
      });
      const state: AgentState = {
        version: 1,
        controllerUrl: ctrl.url,
        caPem: ctrl.caPem,
        caFingerprint: ctrl.fingerprint,
        nodeId: reg.nodeId,
        name: 'w3',
        credential: reg.credential,
      };
      await admin.request('PATCH', '/v1/settings', {
        json: { autoUpdate: 'off', toolchain: { act: null, node: null } },
      });
      const statuses: AgentStatus[] = [];
      const first = await startAgent(agentDir, state, await managedBuild(root, id), statuses);
      await waitFor(async () => {
        const n = await admin.getNode('w3');
        return n.update?.detail?.includes('toolchain') ? n : undefined;
      }, 'the toolchain gap');
      await admin.startUpdate({ nodes: ['w3'] });
      await waitFor(() => statuses.includes('restarting'), 'a restart for the toolchain');
      const act = join(agentDir, 'toolchain', 'act', '9.9.1', 'act');
      expect(existsSync(act)).toBe(true);
      const nodeBin = join(
        agentDir,
        'toolchain',
        'node',
        '9.9.2',
        'node-v9.9.2-linux-x64',
        'bin',
        'node',
      );
      expect(await readInstallState(root)).toMatchObject({
        current: id,
        node: nodeBin,
        pending: { id, previous: id, previousNode: null },
      });
      await first.stop();

      // Restarted without the launcher, so still on the system Node.js: act is managed now, and the
      // next attempt reports that Node.js was not switched instead of restarting in a loop.
      const after: AgentStatus[] = [];
      await startAgent(agentDir, state, await managedBuild(root, id), after);
      const n = await waitFor(async () => {
        const x = await admin.getNode('w3');
        return x.update?.state === 'failed' ? x : undefined;
      }, 'the failed toolchain update');
      expect(n.info?.build?.tools).toMatchObject({ act: '9.9.1', managed: ['act'] });
      expect(n.update?.detail).toContain('the launcher did not switch to Node.js 9.9.2');
      expect(after).not.toContain('restarting');
    },
  );
});

import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { GridstatorClient, readClientConfig, writeClientConfig } from '@gridstator/client';
import {
  Store,
  controllerPaths,
  initController,
  issueClientTokenOffline,
  startController,
  type RunningController,
} from '@gridstator/controller';
import { decodeWsData, hashToken, readJsonFile, writeJsonFile } from '@gridstator/shared';
import { AGENT_PROTOCOL_VERSION, type NodeInfo } from '@gridstator/protocol';
import {
  configureLocalController,
  ensureLocalLogin,
  localControllerService,
  localControllerStatus,
  type LocalControllerOptions,
} from './local-controller.js';

let dir: string;
let dataDir: string;
let configPath: string;
let controller: RunningController | undefined;
let port: number;

function serviceOutput(state = 'active', directory = dataDir): string {
  return `LoadState=loaded\nActiveState=${state}\nExecStart={ path=/usr/bin/node ; argv[]=/usr/bin/node /opt/gridstator.mjs controller start --data-dir ${JSON.stringify(directory)} ; ignore_errors=no ; }\nEnvironment=GRIDSTATOR_LOG_FORMAT=json\nEnvironmentFiles=\n`;
}

async function unusedPort(): Promise<number> {
  const socket = createServer();
  await new Promise<void>((resolve) => socket.listen(0, '127.0.0.1', resolve));
  const address = socket.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  await new Promise<void>((resolve, reject) =>
    socket.close((error) => (error ? reject(error) : resolve())),
  );
  return address.port;
}

function options(): LocalControllerOptions {
  return {
    dataDir,
    configPath,
    execute: async (_file, args) => {
      if (args.includes('show')) return { stdout: serviceOutput(), stderr: '' };
      if (args.includes('restart')) {
        await controller?.close();
        controller = undefined;
        controller = await startController({ dataDir });
      }
      return { stdout: '', stderr: '' };
    },
    verificationTimeoutMs: 500,
  };
}

function tokenCount(): number {
  const store = new Store(controllerPaths(dataDir).db);
  try {
    return store.listTokens().length;
  } finally {
    store.close();
  }
}

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-local-'));
  dataDir = join(dir, 'controller');
  configPath = join(dir, 'client.json');
  port = await unusedPort();
  await initController({ dataDir, host: '127.0.0.1', port });
});
afterEach(async () => {
  await controller?.close();
  controller = undefined;
  await rm(dir, { recursive: true, force: true });
});

describe('shared local access', () => {
  it('creates exactly one admin credential across concurrent windows and reuses it', async () => {
    const before = tokenCount();
    const result = await Promise.all(Array.from({ length: 9 }, () => ensureLocalLogin(options())));
    expect(result.every((entry) => entry.status === 'connected')).toBe(true);
    expect(tokenCount()).toBe(before + 1);
    const first = await readClientConfig(configPath);
    await ensureLocalLogin(options());
    expect(await readClientConfig(configPath)).toEqual(first);
    expect(tokenCount()).toBe(before + 1);
    expect(JSON.stringify(result)).not.toContain(first!.token);
  });

  it('promotes an existing local operator without changing clusters', async () => {
    const issued = await issueClientTokenOffline({ dataDir, name: 'operator', role: 'operator' });
    const publicUrl = `https://192.168.1.77:${port}`;
    await writeClientConfig(
      {
        version: 1,
        url: publicUrl,
        caPem: issued.caPem,
        caFingerprint: issued.fingerprint,
        token: issued.token,
      },
      configPath,
    );
    expect((await ensureLocalLogin(options())).status).toBe('connected');
    const saved = await readClientConfig(configPath);
    expect(saved?.url).toBe(publicUrl);
    expect((await localControllerStatus(options())).url).toBe(publicUrl);
    expect(saved?.token).not.toBe(issued.token);
    const store = new Store(controllerPaths(dataDir).db);
    try {
      expect(store.tokenByHash(hashToken(saved!.token))?.role).toBe('admin');
    } finally {
      store.close();
    }
  });

  it('honors revocation until Restore Local Access is explicitly requested', async () => {
    await ensureLocalLogin(options());
    const saved = (await readClientConfig(configPath))!;
    const publicUrl = `https://192.168.1.77:${port}`;
    await writeClientConfig({ ...saved, url: publicUrl }, configPath);
    const store = new Store(controllerPaths(dataDir).db);
    try {
      store.revokeToken(store.tokenByHash(hashToken(saved.token))!.id, Date.now());
    } finally {
      store.close();
    }
    const count = tokenCount();
    expect((await ensureLocalLogin(options())).status).toBe('revoked');
    expect(tokenCount()).toBe(count);
    expect((await ensureLocalLogin(options(), true)).status).toBe('connected');
    expect(tokenCount()).toBe(count + 1);
    expect((await readClientConfig(configPath))?.url).toBe(publicUrl);
    expect((await localControllerStatus(options())).url).toBe(publicUrl);
  });

  it('preserves an explicit saved connection to another cluster', async () => {
    const foreign = await initController({ dataDir: join(dir, 'foreign') });
    const saved = {
      version: 1 as const,
      url: 'https://other:7300',
      caPem: foreign.caPem,
      caFingerprint: foreign.fingerprint,
      token: foreign.adminToken,
    };
    await writeClientConfig(saved, configPath);
    expect((await ensureLocalLogin(options())).status).toBe('differentCluster');
    expect(await readClientConfig(configPath)).toEqual(saved);
  });

  it('reports local cluster identity and never starts a stopped service while inspecting it', async () => {
    const calls: string[][] = [];
    const status = await localControllerStatus({
      dataDir,
      execute: async (_file, args) => {
        calls.push(args);
        return { stdout: serviceOutput('inactive'), stderr: '' };
      },
    });
    expect(status).toMatchObject({
      available: true,
      service: { installed: true, state: 'inactive' },
    });
    expect(status.clusterId).toBeTruthy();
    expect(status.caFingerprint).toMatch(/^sha256:/);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toContain('show');
  });
});

describe('coordinated host configuration', () => {
  it('refuses service actions and configuration when the installed unit runs another controller', async () => {
    const calls: string[][] = [];
    const before = await readFile(controllerPaths(dataDir).config, 'utf8');
    const wrongUnit: LocalControllerOptions = {
      ...options(),
      execute: async (_file, args) => {
        calls.push(args);
        return { stdout: serviceOutput('active', join(dir, 'other-controller')), stderr: '' };
      },
    };
    const status = await localControllerStatus(wrongUnit);
    expect(status).toMatchObject({
      available: true,
      service: { installed: true, canManage: false },
    });
    expect(status.service.reason).toContain('another data directory');
    await expect(localControllerService(wrongUnit, 'restart')).rejects.toThrow(
      'another data directory',
    );
    await expect(
      configureLocalController(wrongUnit, {
        host: '127.0.0.1',
        port,
        url: `https://127.0.0.1:${port}`,
      }),
    ).rejects.toThrow('another data directory');
    expect(calls.every((args) => args.includes('show'))).toBe(true);
    expect(await readFile(controllerPaths(dataDir).config, 'utf8')).toBe(before);
  });

  it('does not offer controls for a custom controller when the default service has no matching directory', async () => {
    const status = await localControllerStatus({
      dataDir,
      execute: async () => ({
        stdout:
          'LoadState=loaded\nActiveState=active\nExecStart={ path=/usr/bin/node ; argv[]=/usr/bin/node /opt/gridstator.mjs controller start --with-agent ; ignore_errors=no ; }\nEnvironment=GRIDSTATOR_LOG_FORMAT=json\n',
        stderr: '',
      }),
    });
    expect(status.service.canManage).toBe(false);
    expect(status.service.reason).toContain('another data directory');
  });

  it('accepts a matching controller directory declared through the unit environment', async () => {
    const status = await localControllerStatus({
      dataDir,
      execute: async () => ({
        stdout: `LoadState=loaded\nActiveState=inactive\nExecStart={ path=/usr/bin/node ; argv[]=/usr/bin/node /opt/gridstator.mjs controller start ; ignore_errors=no ; }\nEnvironment=GRIDSTATOR_LOG_FORMAT=json ${JSON.stringify(`GRIDSTATOR_CONTROLLER_DATA=${dataDir}`)}\n`,
        stderr: '',
      }),
    });
    expect(status.service).toMatchObject({ installed: true, canManage: true, reason: null });
  });

  it('refuses ambiguous service definitions instead of guessing the target controller', async () => {
    const status = await localControllerStatus({
      dataDir,
      execute: async () => ({
        stdout:
          'LoadState=loaded\nActiveState=active\nExecStart={ path=/bin/bash ; argv[]=/bin/bash -c custom-wrapper ; ignore_errors=no ; }\n',
        stderr: '',
      }),
    });
    expect(status.service.canManage).toBe(false);
    expect(status.service.reason).toContain('Cannot verify');
  });

  it('blocks a service restart while the embedded worker has a job and restores its scheduling mode', async () => {
    controller = await startController({ dataDir });
    await ensureLocalLogin(options());
    const saved = (await readClientConfig(configPath))!;
    const admin = new GridstatorClient({
      url: controller.url,
      ca: saved.caPem,
      token: saved.token,
    });
    const info: NodeInfo = {
      hostname: 'embedded',
      platform: 'linux',
      osRelease: 'test',
      arch: 'x64',
      cpus: 8,
      memoryTotalMb: 16384,
      agentVersion: '0.1.0',
      docker: { available: true },
      buildx: { available: false },
      act: { available: false },
      executors: ['command'],
    };
    const registration = controller.registerLocalAgent('embedded', info);
    const agent = new GridstatorClient({
      url: controller.url,
      ca: saved.caPem,
      token: registration.credential,
    });
    const ws = agent.openWebSocket('/v1/agent/ws');
    const welcome = new Promise<void>((resolve) =>
      ws.on('message', (data) => {
        const message: { type: string } = JSON.parse(decodeWsData(data));
        if (message.type === 'welcome') resolve();
      }),
    );
    const assigned = new Promise<void>((resolve) =>
      ws.on('message', (data) => {
        const message: { type: string } = JSON.parse(decodeWsData(data));
        if (message.type === 'job.assign') resolve();
      }),
    );
    ws.on('open', () =>
      ws.send(
        JSON.stringify({
          type: 'hello',
          protocolVersion: AGENT_PROTOCOL_VERSION,
          info,
          activeJobs: [],
          results: [],
        }),
      ),
    );
    await welcome;
    await writeJsonFile(join(dataDir, 'agent', 'agent.json'), { nodeId: registration.nodeId });
    const job = await admin.submitJob({
      kind: 'command',
      command: ['true'],
      image: 'alpine',
      resources: { cpus: 1, memoryMb: 512 },
    });
    await assigned;
    const calls: string[][] = [];
    try {
      await expect(
        localControllerService(
          {
            ...options(),
            execute: async (_file, args) => {
              calls.push(args);
              return { stdout: serviceOutput(), stderr: '' };
            },
          },
          'restart',
        ),
      ).rejects.toThrow('Finish the 1 running job');
      expect(calls.some((args) => args.includes('restart'))).toBe(false);
      const unchanged = await admin.getJob(job.id);
      expect(unchanged.cancelRequestedAt).toBeNull();
      expect(
        (await admin.listNodes()).find((node) => node.id === registration.nodeId)?.settings
          .schedulingMode,
      ).toBe('enabled');
    } finally {
      ws.close();
      agent.close();
      admin.close();
    }
  });

  it('keeps the previous listener available for offline and legacy workers', async () => {
    controller = await startController({ dataDir });
    await ensureLocalLogin(options());
    const previous = controller.url;
    const targetPort = await unusedPort();
    const result = await configureLocalController(options(), {
      host: '127.0.0.1',
      port: targetPort,
      url: `https://127.0.0.1:${targetPort}`,
    });
    expect(result.aliases).toContainEqual({ host: '127.0.0.1', port });
    const saved = (await readClientConfig(configPath))!;
    expect(saved.url).toBe(`https://127.0.0.1:${targetPort}`);
    const legacy = new GridstatorClient({ url: previous, ca: saved.caPem, token: saved.token });
    try {
      expect((await legacy.cluster()).clusterId).toBe(result.clusterId);
    } finally {
      legacy.close();
    }
  });

  it('supports an explicit Apply & Restart while the service is stopped', async () => {
    let active = false;
    const standard = options();
    const stopped: LocalControllerOptions = {
      ...standard,
      execute: async (file, args) => {
        if (args.includes('show'))
          return {
            stdout: serviceOutput(active ? 'active' : 'inactive'),
            stderr: '',
          };
        const result = await standard.execute!(file, args);
        if (args.includes('restart')) active = true;
        return result;
      },
    };
    const targetPort = await unusedPort();
    const updated = await configureLocalController(stopped, {
      host: '127.0.0.1',
      port: targetPort,
      url: `https://127.0.0.1:${targetPort}`,
    });
    expect(updated.service.state).toBe('active');
    expect(updated.port).toBe(targetPort);
  });

  it('requires a fresh online worker connection even when its previous endpoint is unchanged', async () => {
    controller = await startController({ dataDir });
    await ensureLocalLogin(options());
    const saved = (await readClientConfig(configPath))!;
    const info: NodeInfo = {
      hostname: 'worker',
      controllerUrl: controller.url,
      platform: 'linux',
      osRelease: 'test',
      arch: 'x64',
      cpus: 8,
      memoryTotalMb: 16384,
      agentVersion: '0.1.0',
      docker: { available: true },
      buildx: { available: false },
      act: { available: false },
      executors: ['command'],
    };
    const registration = controller.registerLocalAgent('worker', info);
    const agent = new GridstatorClient({
      url: controller.url,
      ca: saved.caPem,
      token: registration.credential,
    });
    const ws = agent.openWebSocket('/v1/agent/ws');
    const welcome = new Promise<void>((resolve) =>
      ws.on('message', (data) => {
        const message: { type: string } = JSON.parse(decodeWsData(data));
        if (message.type === 'welcome') resolve();
      }),
    );
    ws.on('open', () =>
      ws.send(
        JSON.stringify({
          type: 'hello',
          protocolVersion: AGENT_PROTOCOL_VERSION,
          info,
          activeJobs: [],
          results: [],
        }),
      ),
    );
    await welcome;
    try {
      // Both loopback addresses can listen on the same port. The retained old endpoint stays
      // reachable, but this fake worker deliberately never reconnects after the service restart.
      await expect(
        configureLocalController(options(), { host: '127.0.0.2', port, url: controller.url }),
      ).rejects.toThrow('workers did not reconnect');
      expect((await localControllerStatus(options())).host).toBe('127.0.0.1');
    } finally {
      ws.close();
      agent.close();
    }
  });

  it('restores the previous configuration and service when the new listener cannot bind', async () => {
    controller = await startController({ dataDir });
    await ensureLocalLogin(options());
    const original = await readFile(controllerPaths(dataDir).config, 'utf8');
    const busy = createServer();
    await new Promise<void>((resolve) => busy.listen(0, '127.0.0.1', resolve));
    const address = busy.address();
    if (!address || typeof address === 'string') throw new Error('No test port');
    try {
      await expect(
        configureLocalController(options(), {
          host: '127.0.0.1',
          port: address.port,
          url: `https://127.0.0.1:${address.port}`,
        }),
      ).rejects.toThrow('previous configuration was restored');
      expect(await readFile(controllerPaths(dataDir).config, 'utf8')).toBe(original);
      const saved = (await readClientConfig(configPath))!;
      const recovered = new GridstatorClient({
        url: saved.url,
        ca: saved.caPem,
        token: saved.token,
      });
      try {
        expect((await recovered.health()).ok).toBe(true);
      } finally {
        recovered.close();
      }
    } finally {
      await new Promise<void>((resolve) => busy.close(() => resolve()));
    }
  });

  it('does not mutate configuration when the public URL has the wrong port', async () => {
    const before = await readJsonFile(controllerPaths(dataDir).config);
    await expect(
      configureLocalController(options(), {
        host: '127.0.0.1',
        port: 7400,
        url: 'https://127.0.0.1:7500',
      }),
    ).rejects.toThrow('configured listening port');
    expect(await readJsonFile(controllerPaths(dataDir).config)).toEqual(before);
  });

  it('requires ownership before executing service actions', async () => {
    const calls: string[][] = [];
    await expect(
      localControllerService(
        {
          dataDir: join(dir, 'missing'),
          execute: async (_file, args) => {
            calls.push(args);
            return { stdout: serviceOutput(), stderr: '' };
          },
        },
        'start',
      ),
    ).rejects.toThrow('does not own');
    expect(calls.every((args) => args.includes('show'))).toBe(true);
  });
});

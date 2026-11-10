import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { createServer, type Server } from 'node:https';
import { afterEach, expect, it, vi } from 'vitest';
import { WebSocketServer, type WebSocket } from 'ws';
import { GridstatorClient } from '@gridstator/client';
import { initController, startController, type RunningController } from '@gridstator/controller';
import { DockerCli } from '@gridstator/executors';
import { decodeWsData } from '@gridstator/shared';
import { JobSpecSchema, defaultNodeSettings } from '@gridstator/protocol';
import { generatePki } from '../../controller/src/pki.js';
import { Agent } from './agent.js';
import { readAgentState, updateAgentState, writeAgentState, type AgentState } from './state.js';
import * as stateFiles from './state.js';

let directory: string | undefined;
let controller: RunningController | undefined;
let client: GridstatorClient | undefined;
let agent: Agent | undefined;
let server: Server | undefined;
let websocketServer: WebSocketServer | undefined;
const sockets: WebSocket[] = [];
afterEach(async () => {
  await agent?.stop();
  for (const socket of sockets.splice(0)) socket.terminate();
  if (websocketServer)
    await new Promise<void>((resolve) => websocketServer!.close(() => resolve()));
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  client?.close();
  await controller?.close();
  if (directory) await rm(directory, { recursive: true, force: true });
  agent = undefined;
  client = undefined;
  controller = undefined;
  directory = undefined;
  websocketServer = undefined;
  server = undefined;
  vi.restoreAllMocks();
});

it('drops queued frames and welcome continuations from a socket replaced during name persistence', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-agent-stale-rename-'));
  const pki = await generatePki(directory, 'rename-socket-test');
  server = createServer({ key: pki.serverKeyPem, cert: pki.serverChainPem });
  websocketServer = new WebSocketServer({ server });
  await new Promise<void>((resolve) => server!.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Expected TCP listener');
  const state: AgentState = {
    version: 1,
    controllerUrl: `https://127.0.0.1:${address.port}`,
    caPem: pki.caPem,
    caFingerprint: pki.fingerprint,
    nodeId: 'worker-id',
    name: 'original',
    credential: 'gsa_fixture',
  };
  const dataDir = join(directory, 'agent');
  await writeAgentState(dataDir, state);
  let releasePersist!: () => void;
  const persistGate = new Promise<void>((resolve) => {
    releasePersist = resolve;
  });
  let enteredPersist!: () => void;
  const persistEntered = new Promise<void>((resolve) => {
    enteredPersist = resolve;
  });
  const persist = stateFiles.updateAgentState;
  vi.spyOn(stateFiles, 'updateAgentState').mockImplementation(async (path, current, update) => {
    if (update(current).name === 'stale-name') {
      enteredPersist();
      await persistGate;
    }
    return persist(path, current, update);
  });
  const statuses: string[] = [];
  const freshFrames: unknown[] = [];
  let firstHello!: () => void;
  let secondHello!: () => void;
  const firstConnected = new Promise<void>((resolve) => {
    firstHello = resolve;
  });
  const secondConnected = new Promise<void>((resolve) => {
    secondHello = resolve;
  });
  websocketServer.on('connection', (socket) => {
    sockets.push(socket);
    const first = sockets.length === 1;
    socket.on('message', (bytes) => {
      const message: { type?: string } = JSON.parse(decodeWsData(bytes));
      if (!first) freshFrames.push(message);
      if (message.type === 'hello') (first ? firstHello : secondHello)();
    });
  });
  const welcome = (name: string) => ({
    type: 'welcome',
    nodeId: state.nodeId,
    name,
    settings: defaultNodeSettings({ cpus: 2, memoryTotalMb: 4096 }),
    resume: [],
  });
  try {
    agent = new Agent({ dataDir, state, docker: new DockerCli(join(directory, 'missing-docker')) });
    agent.onStatus((status) => statuses.push(status));
    await agent.start();
    await firstConnected;
    const oldSocket = sockets[0]!;
    oldSocket.send(JSON.stringify(welcome('stale-name')));
    oldSocket.send(
      JSON.stringify({
        type: 'job.assign',
        jobId: 'stale-job',
        assignmentId: 'stale-assignment',
        attempt: 1,
        snapshotId: null,
        spec: JobSpecSchema.parse({ kind: 'command', image: 'alpine', command: ['true'] }),
      }),
    );
    await persistEntered;
    oldSocket.terminate();
    await secondConnected;
    releasePersist();
    const queued = Reflect.get(agent, 'incomingMessages');
    if (!(queued instanceof Promise)) throw new Error('Expected pending incoming frame processing');
    await queued;
    const freshSocket = sockets[1]!;
    const flushed = once(freshSocket, 'pong');
    freshSocket.ping('queue-flushed');
    await flushed;
    expect(statuses).not.toContain('connected');
    expect(freshFrames).toEqual([expect.objectContaining({ type: 'hello' })]);
    freshSocket.send(JSON.stringify(welcome('big-box')));
    await expect.poll(() => statuses.at(-1)).toBe('connected');
    await expect.poll(async () => (await readAgentState(dataDir))?.name).toBe('big-box');
  } finally {
    releasePersist();
  }
});

it('persists the controller name on welcome and live rename without replacing credentials', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-agent-rename-'));
  const data = join(directory, 'controller');
  const initial = await initController({ dataDir: data, host: '127.0.0.1', port: 0 });
  controller = await startController({ dataDir: data });
  client = new GridstatorClient({
    url: controller.url,
    ca: controller.caPem,
    token: initial.adminToken,
  });
  const registration = controller.registerLocalAgent('DESKTOP-JEL', {
    hostname: 'original-hostname',
    platform: 'linux',
    osRelease: '6',
    arch: 'x64',
    cpus: 2,
    memoryTotalMb: 4096,
    agentVersion: '0.1.0',
    docker: { available: false },
    buildx: { available: false },
    act: { available: false },
    executors: [],
  });
  const state: AgentState = {
    version: 1,
    controllerUrl: controller.url,
    caPem: controller.caPem,
    caFingerprint: controller.fingerprint,
    nodeId: registration.nodeId,
    name: 'stale-local-name',
    credential: registration.credential,
  };
  const dataDir = join(directory, 'agent');
  await writeAgentState(dataDir, state);
  agent = new Agent({ dataDir, state, docker: new DockerCli(join(directory, 'missing-docker')) });
  await agent.start();
  await expect.poll(async () => (await readAgentState(dataDir))?.name).toBe('DESKTOP-JEL');
  const before = await client.getNode(registration.nodeId);
  await client.updateNode(registration.nodeId, { name: 'big-box' });
  await expect.poll(async () => (await readAgentState(dataDir))?.name).toBe('big-box');
  expect(state.name).toBe('big-box');
  expect(await readAgentState(dataDir)).toEqual({ ...state, name: 'big-box' });
  expect((await readAgentState(dataDir))?.credential).toBe(registration.credential);
  expect((await readAgentState(dataDir))?.nodeId).toBe(registration.nodeId);
  const after = await client.getNode('big-box');
  expect(after.id).toBe(registration.nodeId);
  expect(after.connectedAt).toBe(before.connectedAt);
  expect(after.info?.hostname).toBe(before.info?.hostname);
});

it('serializes name and address changes and refuses identity replacement', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-agent-state-'));
  const state: AgentState = {
    version: 1,
    nodeId: 'node',
    name: 'old',
    credential: 'gsa_fixture',
    controllerUrl: 'https://old:7300',
    caPem: 'fixture',
    caFingerprint: 'fixture',
  };
  await writeAgentState(directory, state);
  await Promise.all([
    updateAgentState(directory, state, (current) => ({ ...current, name: 'big-box' })),
    updateAgentState(directory, state, (current) => ({
      ...current,
      controllerUrl: 'https://new:7300',
    })),
  ]);
  expect(await readAgentState(directory)).toEqual({
    ...state,
    name: 'big-box',
    controllerUrl: 'https://new:7300',
  });
  await expect(
    updateAgentState(directory, state, (current) => ({ ...current, nodeId: 'another-node' })),
  ).rejects.toMatchObject({ code: 'FORBIDDEN' });
  expect((await readAgentState(directory))?.nodeId).toBe('node');
});

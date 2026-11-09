import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { z } from 'zod';
import type { WebSocket } from 'ws';
import { GridstatorError, decodeWsData } from '@gridstator/shared';
import {
  AGENT_PROTOCOL_VERSION,
  type AgentMessageSchema,
  ControllerMessageSchema,
  serializeManifest,
  type ControllerMessage,
  type Job,
  type JobSpecInput,
  type JobState,
  type NodeInfo,
} from '@gridstator/protocol';
import { GridstatorClient } from '@gridstator/client';
import { digestOf, packBlobs } from '@gridstator/snapshot';
import { initController, startController, type RunningController } from './controller.js';

const TIMING = {
  heartbeatTimeoutMs: 5_000,
  lostTimeoutMs: 1_200,
  cancelTimeoutMs: 1_000,
  assignTimeoutMs: 1_000,
  sweepIntervalMs: 100,
  scheduleIntervalMs: 200,
};

const INFO: NodeInfo = {
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
};

const SPEC: JobSpecInput = {
  kind: 'command',
  image: 'alpine',
  command: ['true'],
  resources: { cpus: 1, memoryMb: 512 },
};

let dir: string;
let ctrl: RunningController;
let admin: GridstatorClient;
const agents: FakeAgent[] = [];

async function waitFor<T>(
  fn: () => Promise<T | undefined> | T | undefined,
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

async function waitForState(id: string, state: JobState, timeoutMs = 5000): Promise<Job> {
  return waitFor(
    async () => {
      const j = await admin.getJob(id);
      return j.state === state ? j : undefined;
    },
    `job ${id} to be ${state}`,
    timeoutMs,
  );
}

class FakeAgent {
  readonly received: ControllerMessage[] = [];
  ws!: WebSocket;
  readonly client: GridstatorClient;

  constructor(
    readonly nodeId: string,
    credential: string,
  ) {
    this.client = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: credential });
  }

  async connect(
    hello: Partial<Extract<z.input<typeof AgentMessageSchema>, { type: 'hello' }>> = {},
  ): Promise<Extract<ControllerMessage, { type: 'welcome' }>> {
    this.received.length = 0;
    this.ws = this.client.openWebSocket('/v1/agent/ws');
    this.ws.on('message', (d) =>
      this.received.push(ControllerMessageSchema.parse(JSON.parse(decodeWsData(d)))),
    );
    await new Promise<void>((resolve, reject) => {
      this.ws.once('open', () => resolve());
      this.ws.once('error', reject);
    });
    this.send({
      type: 'hello',
      protocolVersion: AGENT_PROTOCOL_VERSION,
      info: INFO,
      activeJobs: [],
      results: [],
      ...hello,
    });
    return this.next('welcome');
  }

  send(msg: z.input<typeof AgentMessageSchema>): void {
    this.ws.send(JSON.stringify(msg));
  }

  async next<T extends ControllerMessage['type']>(
    type: T,
    pred: (m: Extract<ControllerMessage, { type: T }>) => boolean = () => true,
  ): Promise<Extract<ControllerMessage, { type: T }>> {
    return waitFor<Extract<ControllerMessage, { type: T }>>(() => {
      const i = this.received.findIndex(
        (m) => m.type === type && pred(m as Extract<ControllerMessage, { type: T }>),
      );
      if (i === -1) return undefined;
      return this.received.splice(i, 1)[0] as Extract<ControllerMessage, { type: T }> | undefined;
    }, `${type} message`);
  }

  close(): void {
    this.ws?.terminate();
    this.client.close();
  }
}

async function addAgent(name: string): Promise<FakeAgent> {
  const reg = ctrl.registerLocalAgent(name, INFO);
  const a = new FakeAgent(reg.nodeId, reg.credential);
  agents.push(a);
  return a;
}

async function start(): Promise<void> {
  ctrl = await startController({ dataDir: dir, host: '127.0.0.1', port: 0, timing: TIMING });
}

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-ctrl-'));
  const init = await initController({ dataDir: dir, host: '127.0.0.1', port: 0 });
  await start();
  admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: init.adminToken });
});

afterEach(async () => {
  for (const a of agents.splice(0)) a.close();
  admin.close();
  await ctrl.close();
  await rm(dir, { recursive: true, force: true });
});

describe('authentication', () => {
  it('serves health and CA publicly but requires tokens elsewhere', async () => {
    const anon = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: 'gsc_invalid' });
    expect((await anon.health()).ok).toBe(true);
    await expect(anon.listJobs()).rejects.toMatchObject({ code: 'UNAUTHORIZED' });
    anon.close();
  });

  it('enforces token kinds and roles, and revocation', async () => {
    const op = await admin.createToken({ kind: 'client', name: 'op', role: 'operator' });
    const opClient = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: op.token });
    expect(await opClient.listJobs()).toEqual([]);
    await expect(opClient.createToken({ kind: 'client', name: 'x' })).rejects.toMatchObject({
      code: 'FORBIDDEN',
    });

    const join = await admin.createToken({ kind: 'join', name: 'j' });
    const joinClient = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: join.token });
    await expect(joinClient.listJobs()).rejects.toMatchObject({ code: 'FORBIDDEN' });

    await admin.revokeToken(op.info.id);
    await expect(opClient.listJobs()).rejects.toMatchObject({ code: 'UNAUTHORIZED' });
    opClient.close();
    joinClient.close();
  });

  it('registers agents with single-use join tokens', async () => {
    const join = await admin.createToken({ kind: 'join', name: 'j' });
    const c = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: join.token });
    const reg = await c.registerAgent(join.token, { name: 'w1', info: INFO });
    expect(reg.credential).toMatch(/^gsa_/);
    await expect(c.registerAgent(join.token, { name: 'w2', info: INFO })).rejects.toMatchObject({
      code: 'UNAUTHORIZED',
    });
    c.close();
  });
});

describe('job lifecycle', () => {
  it('runs a job end to end with idempotent log handling', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const assign = await agent.next('job.assign');
    expect(assign.jobId).toBe(job.id);
    const base = { jobId: job.id, assignmentId: assign.assignmentId };
    agent.send({ type: 'job.state', ...base, phase: 'starting', ts: Date.now() });
    agent.send({ type: 'job.state', ...base, phase: 'running', ts: Date.now() });
    const entries = [
      { s: 1, t: Date.now(), o: 'stdout' as const, d: 'one\n' },
      { s: 2, t: Date.now(), o: 'stderr' as const, d: 'two\n' },
    ];
    agent.send({ type: 'job.log', ...base, entries });
    agent.send({ type: 'job.log', ...base, entries }); // duplicate delivery
    agent.send({
      type: 'job.log',
      jobId: job.id,
      assignmentId: 'stale',
      entries: [{ s: 3, t: 0, o: 'stdout', d: 'stale\n' }],
    });
    await waitForState(job.id, 'running');
    agent.send({
      type: 'job.result',
      result: {
        ...base,
        outcome: 'succeeded',
        exitCode: 0,
        error: null,
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 2,
      },
    });
    await agent.next('job.ack', (m) => m.jobId === job.id);
    const done = await waitForState(job.id, 'succeeded');
    expect(done.exitCode).toBe(0);
    const logs = await admin.jobLogs(job.id);
    expect(logs.entries.map((e) => e.d)).toEqual(['one\n', 'two\n']);
    expect(logs.complete).toBe(true);
    const events = await admin.jobEvents(job.id);
    expect(events.map((e) => e.toState).filter(Boolean)).toEqual([
      'created',
      'queued',
      'assigned',
      'starting',
      'running',
      'succeeded',
    ]);
    // A duplicate result is acknowledged but changes nothing.
    agent.send({
      type: 'job.result',
      result: {
        ...base,
        outcome: 'failed',
        exitCode: 1,
        error: 'x',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 2,
      },
    });
    await agent.next('job.ack', (m) => m.jobId === job.id);
    expect((await admin.getJob(job.id)).state).toBe('succeeded');
  });

  it('streams logs to followers and ends the stream when the job finishes', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const assign = await agent.next('job.assign');
    const base = { jobId: job.id, assignmentId: assign.assignmentId };
    agent.send({ type: 'job.log', ...base, entries: [{ s: 1, t: 1, o: 'stdout', d: 'early\n' }] });
    const seen: string[] = [];
    const following = admin.followLogs(job.id, (es) => es.forEach((e) => seen.push(e.d)));
    await waitFor(() => seen.length === 1 || undefined, 'backlog');
    agent.send({ type: 'job.log', ...base, entries: [{ s: 2, t: 2, o: 'stdout', d: 'late\n' }] });
    agent.send({
      type: 'job.result',
      result: {
        ...base,
        outcome: 'failed',
        exitCode: 2,
        error: 'exit 2',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 2,
      },
    });
    expect(await following).toBe('failed');
    expect(seen).toEqual(['early\n', 'late\n']);
  });

  it('cancels queued jobs immediately and running jobs via the agent', async () => {
    const queued = await admin.submitJob({
      ...SPEC,
      target: { node: undefined, labels: ['nowhere'] },
    });
    expect((await admin.cancelJob(queued.id)).state).toBe('cancelled');

    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const assign = await agent.next('job.assign');
    agent.send({
      type: 'job.state',
      jobId: job.id,
      assignmentId: assign.assignmentId,
      phase: 'running',
      ts: Date.now(),
    });
    await waitForState(job.id, 'running');
    const pending = await admin.cancelJob(job.id);
    expect(pending.cancelRequestedAt).not.toBeNull();
    await agent.next('job.cancel', (m) => m.jobId === job.id);
    // Even a non-zero exit after a cancel request is recorded as cancelled.
    agent.send({
      type: 'job.result',
      result: {
        jobId: job.id,
        assignmentId: assign.assignmentId,
        outcome: 'failed',
        exitCode: 143,
        error: 'killed',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
      },
    });
    await waitForState(job.id, 'cancelled');
  });

  it('finalizes a cancellation the agent never confirms', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const assign = await agent.next('job.assign');
    agent.send({
      type: 'job.state',
      jobId: job.id,
      assignmentId: assign.assignmentId,
      phase: 'running',
      ts: Date.now(),
    });
    await waitForState(job.id, 'running');
    await admin.cancelJob(job.id);
    await waitForState(job.id, 'cancelled', 4000);
  });

  it('retries manually and automatically, preserving lineage', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob({ ...SPEC, retry: { max: 1, on: ['failed'] } });
    let assign = await agent.next('job.assign');
    agent.send({
      type: 'job.result',
      result: {
        jobId: job.id,
        assignmentId: assign.assignmentId,
        outcome: 'failed',
        exitCode: 1,
        error: 'boom',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
      },
    });
    await waitForState(job.id, 'failed');
    assign = await agent.next('job.assign');
    expect(assign.jobId).not.toBe(job.id);
    expect(assign.attempt).toBe(2);
    const auto = await admin.getJob(assign.jobId);
    expect(auto.retryOf).toBe(job.id);
    agent.send({
      type: 'job.result',
      result: {
        jobId: auto.id,
        assignmentId: assign.assignmentId,
        outcome: 'failed',
        exitCode: 1,
        error: 'boom',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
      },
    });
    await waitForState(auto.id, 'failed');
    await new Promise((r) => setTimeout(r, 300));
    expect(agent.received.some((m) => m.type === 'job.assign')).toBe(false); // retries exhausted

    const manual = await admin.retryJob(auto.id);
    expect(manual).toMatchObject({ attempt: 3, retryOf: auto.id, state: 'queued' });
    await expect(admin.retryJob(manual.id)).rejects.toMatchObject({ code: 'CONFLICT' });
  });

  it('requeues a rejected job onto another node', async () => {
    const a = await addAgent('a');
    const b = await addAgent('b');
    await a.connect();
    const job = await admin.submitJob({ ...SPEC, target: { node: undefined, labels: [] } });
    const first = await a.next('job.assign');
    await b.connect();
    a.send({
      type: 'job.rejected',
      jobId: job.id,
      assignmentId: first.assignmentId,
      reason: 'disk full',
    });
    const second = await b.next('job.assign');
    expect(second.jobId).toBe(job.id);
    expect(second.assignmentId).not.toBe(first.assignmentId);
  });

  it('does not schedule onto draining nodes', async () => {
    const a = await addAgent('a');
    await a.connect();
    await admin.updateNode('a', { schedulingMode: 'draining' });
    const job = await admin.submitJob(SPEC);
    await waitFor(
      async () => (await admin.getJob(job.id)).stateReason === 'node a is draining' || undefined,
      'pending reason',
    );
    await admin.updateNode('a', { schedulingMode: 'enabled' });
    expect((await a.next('job.assign')).jobId).toBe(job.id);
  });

  it('rejects unknown snapshots and unknown target nodes', async () => {
    await expect(admin.submitJob({ ...SPEC, snapshot: digestOf('nope') })).rejects.toMatchObject({
      code: 'BAD_REQUEST',
    });
    await expect(
      admin.submitJob({ ...SPEC, target: { node: 'ghost', labels: [] } }),
    ).rejects.toMatchObject({ code: 'NOT_FOUND' });
  });
});

describe('failure handling and reconciliation', () => {
  it('marks running jobs lost and requeues assigned jobs when a node disappears', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const running = await admin.submitJob(SPEC);
    const a1 = await agent.next('job.assign');
    agent.send({
      type: 'job.state',
      jobId: running.id,
      assignmentId: a1.assignmentId,
      phase: 'running',
      ts: Date.now(),
    });
    await waitForState(running.id, 'running');
    const assigned = await admin.submitJob(SPEC);
    await agent.next('job.assign', (m) => m.jobId === assigned.id);
    agent.close();
    const lost = await waitForState(running.id, 'lost', 5000);
    expect(lost.error).toMatch(/Lost contact/);
    const requeued = await waitForState(assigned.id, 'queued', 5000);
    expect(requeued.nodeId).toBeNull();
  });

  it('reconciles in-flight jobs after a controller restart', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const keep = await admin.submitJob(SPEC);
    const ak = await agent.next('job.assign', (m) => m.jobId === keep.id);
    const forget = await admin.submitJob(SPEC);
    const af = await agent.next('job.assign', (m) => m.jobId === forget.id);
    for (const [j, a] of [
      [keep, ak],
      [forget, af],
    ] as const) {
      agent.send({
        type: 'job.state',
        jobId: j.id,
        assignmentId: a.assignmentId,
        phase: 'running',
        ts: Date.now(),
      });
    }
    agent.send({
      type: 'job.log',
      jobId: keep.id,
      assignmentId: ak.assignmentId,
      entries: [{ s: 1, t: 1, o: 'stdout', d: 'before\n' }],
    });
    await waitForState(keep.id, 'running');
    await waitForState(forget.id, 'running');
    await waitFor(
      async () => (await admin.getJob(keep.id)).logSeq === 1 || undefined,
      'log persisted',
    );

    agent.close();
    const token = (
      await admin.createToken({ kind: 'client', name: 'after-restart', role: 'admin' })
    ).token;
    admin.close();
    await ctrl.close();
    await start();
    admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token });

    // The agent comes back still running `keep`, with an unknown job, and without `forget`.
    const again = new FakeAgent(agent.nodeId, ctrl.registerLocalAgent('n1', INFO).credential);
    agents.push(again);
    const welcome = await again.connect({
      activeJobs: [
        { jobId: keep.id, assignmentId: ak.assignmentId, phase: 'running', logSeq: 2 },
        { jobId: '01ZZZZZZZZZZZZZZZZZZZZZZZZ', assignmentId: 'x', phase: 'running', logSeq: 0 },
      ],
    });
    expect(welcome.resume).toEqual([{ jobId: keep.id, logSeq: 1 }]);
    await again.next('job.cancel', (m) => m.jobId === '01ZZZZZZZZZZZZZZZZZZZZZZZZ');
    expect((await waitForState(forget.id, 'lost')).error).toMatch(/restarted or lost track/);

    // Replayed + new log entries, then the result.
    again.send({
      type: 'job.log',
      jobId: keep.id,
      assignmentId: ak.assignmentId,
      entries: [
        { s: 1, t: 1, o: 'stdout', d: 'before\n' },
        { s: 2, t: 2, o: 'stdout', d: 'during-outage\n' },
      ],
    });
    again.send({
      type: 'job.result',
      result: {
        jobId: keep.id,
        assignmentId: ak.assignmentId,
        outcome: 'succeeded',
        exitCode: 0,
        error: null,
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 2,
      },
    });
    await waitForState(keep.id, 'succeeded');
    expect((await admin.jobLogs(keep.id)).entries.map((e) => e.d)).toEqual([
      'before\n',
      'during-outage\n',
    ]);
  });

  it('applies results the agent finished while disconnected', async () => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const a = await agent.next('job.assign');
    agent.send({
      type: 'job.state',
      jobId: job.id,
      assignmentId: a.assignmentId,
      phase: 'running',
      ts: Date.now(),
    });
    await waitForState(job.id, 'running');
    agent.close();
    const back = new FakeAgent(agent.nodeId, ctrl.registerLocalAgent('n1', INFO).credential);
    agents.push(back);
    const result = {
      jobId: job.id,
      assignmentId: a.assignmentId,
      outcome: 'succeeded' as const,
      exitCode: 0,
      error: null,
      oomKilled: false,
      finishedAt: Date.now(),
      logSeq: 0,
    };
    const welcome = await back.connect({ results: [result] });
    expect(welcome.resume).toEqual([{ jobId: job.id, logSeq: 0 }]);
    back.send({ type: 'job.result', result });
    await waitForState(job.id, 'succeeded');
  });
});

describe('snapshots and blobs', () => {
  it('accepts verified uploads and rejects mismatched or incomplete ones', async () => {
    const content = Buffer.from('console.log(1)\n');
    const d = digestOf(content);
    const manifest = Buffer.from(
      serializeManifest({
        version: 1,
        entries: [
          { path: 'index.js', type: 'file', size: content.length, digest: d, executable: false },
        ],
      }),
    );
    const m = digestOf(manifest);
    expect(await admin.missingBlobs([d, m])).toEqual([d, m]);
    await expect(admin.createSnapshot(digestOf('none'))).rejects.toMatchObject({
      code: 'NOT_FOUND',
    });

    // Manifest uploaded without its file: registration must fail.
    await admin.uploadBlobs(
      packBlobs([
        {
          digest: m,
          size: manifest.length,
          open: async function* () {
            yield manifest;
          },
        },
      ]),
    );
    await expect(admin.createSnapshot(m)).rejects.toMatchObject({ code: 'BAD_REQUEST' });

    // Content that does not match its claimed digest is refused.
    await expect(
      admin.uploadBlobs(
        packBlobs([
          {
            digest: d,
            size: 3,
            open: async function* () {
              yield Buffer.from('bad');
            },
          },
        ]),
      ),
    ).rejects.toBeInstanceOf(GridstatorError);

    const res = await admin.uploadBlobs(
      packBlobs([
        {
          digest: d,
          size: content.length,
          open: async function* () {
            yield content;
          },
        },
      ]),
    );
    expect(res.stored).toBe(1);
    const snap = await admin.createSnapshot(m);
    expect(snap).toMatchObject({ id: m, fileCount: 1, totalBytes: content.length });
    const job = await admin.submitJob({ ...SPEC, snapshot: m });
    expect(job.snapshotId).toBe(m);
  });

  it('rejects manifests with unsafe paths', async () => {
    const d = digestOf('x');
    await admin.uploadBlobs(
      packBlobs([
        {
          digest: d,
          size: 1,
          open: async function* () {
            yield Buffer.from('x');
          },
        },
      ]),
    );
    const bad = Buffer.from(
      JSON.stringify({
        version: 1,
        entries: [
          { path: '../../etc/passwd', type: 'file', size: 1, digest: d, executable: false },
        ],
      }),
    );
    const bd = digestOf(bad);
    await admin.uploadBlobs(
      packBlobs([
        {
          digest: bd,
          size: bad.length,
          open: async function* () {
            yield bad;
          },
        },
      ]),
    );
    await expect(admin.createSnapshot(bd)).rejects.toThrow(/".." path segment/);
  });
});

describe('outputs and artifacts', () => {
  it.each([1200, -1])('records steps and artifacts (%i ms)', async (durationMs) => {
    const agent = await addAgent('n1');
    await agent.connect();
    const job = await admin.submitJob(SPEC);
    const assign = await agent.next('job.assign');
    const ids = { jobId: job.id, assignmentId: assign.assignmentId };
    agent.send({ type: 'job.state', ...ids, phase: 'running', ts: Date.now() });
    const step = {
      id: 's1',
      group: 'CI/test',
      name: 'Run tests',
      status: 'running' as const,
      startedAt: Date.now(),
      durationMs: null,
    };
    agent.send({ type: 'job.outputs', ...ids, outputs: { steps: [step] } });
    await waitFor(
      async () =>
        (await admin.getJob(job.id)).outputs?.steps?.[0]?.name === 'Run tests' || undefined,
      'live outputs',
    );
    agent.send({
      type: 'job.outputs',
      ...ids,
      outputs: { steps: [{ ...step, status: 'success', durationMs }] },
    });
    await waitFor(
      async () =>
        (await admin.getJob(job.id)).outputs?.steps?.[0]?.durationMs === Math.max(0, durationMs) ||
        undefined,
      'completed step timing',
    );

    const content = Buffer.from('<testsuites/>');
    const digest = digestOf(content);
    // Agents upload artifact blobs with their own credential.
    expect(await agent.client.missingBlobs([digest])).toEqual([digest]);
    await agent.client.uploadBlobs(
      packBlobs([
        {
          digest,
          size: content.length,
          open: async function* () {
            yield content;
          },
        },
      ]),
    );
    agent.send({
      type: 'job.result',
      result: {
        ...ids,
        outcome: 'succeeded',
        exitCode: 0,
        error: null,
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
        outputs: { steps: [{ ...step, status: 'success', durationMs }] },
        artifacts: [{ name: 'reports/junit.xml', digest, size: content.length }],
      },
    });
    const done = await waitForState(job.id, 'succeeded');
    expect(done.outputs?.steps?.[0]).toMatchObject({
      status: 'success',
      durationMs: Math.max(0, durationMs),
    });
    expect(done.artifacts).toEqual([{ name: 'reports/junit.xml', digest, size: content.length }]);
    expect(await admin.listArtifacts(job.id)).toHaveLength(1);
    const res = await admin.downloadArtifact(job.id, 'reports/junit.xml');
    const chunks: Buffer[] = [];
    for await (const c of res) chunks.push(c as Buffer);
    expect(Buffer.concat(chunks).toString()).toBe('<testsuites/>');
    await expect(admin.downloadArtifact(job.id, 'nope.txt')).rejects.toMatchObject({
      code: 'NOT_FOUND',
    });
    // Outputs from a stale assignment are ignored.
    agent.send({
      type: 'job.outputs',
      jobId: job.id,
      assignmentId: 'stale',
      outputs: { steps: [] },
    });
    await new Promise((r) => setTimeout(r, 200));
    expect((await admin.getJob(job.id)).outputs?.steps).toHaveLength(1);
  });
});

describe('job groups (sharding)', () => {
  const finish = (
    a: FakeAgent,
    m: { jobId: string; assignmentId: string },
    outcome: 'succeeded' | 'failed',
  ) =>
    a.send({
      type: 'job.result',
      result: {
        ...m,
        outcome,
        exitCode: outcome === 'succeeded' ? 0 : 1,
        error: outcome === 'succeeded' ? null : 'boom',
        oomKilled: false,
        finishedAt: Date.now(),
        logSeq: 0,
      },
    });

  it('spreads shards across nodes, substitutes placeholders and aggregates state', async () => {
    const a = await addAgent('a');
    const b = await addAgent('b');
    await a.connect();
    await b.connect();
    const group = await admin.submitGroup({
      spec: { ...SPEC, command: ['sh', '-c', 'run --shard={shard}/{total}'] },
      shards: 2,
    });
    expect(group).toMatchObject({ shardTotal: 2, state: 'queued' });
    expect(group.jobs.map((j) => [j.shardIndex, j.shardTotal, j.groupId])).toEqual([
      [1, 2, group.id],
      [2, 2, group.id],
    ]);
    const onA = await a.next('job.assign');
    const onB = await b.next('job.assign');
    const assigned = [onA, onB]
      .map((m) => (m.spec.kind === 'command' ? m.spec.command[2] : ''))
      .sort();
    expect(assigned).toEqual(['run --shard=1/2', 'run --shard=2/2']);
    expect(onA.spec.env['GRIDSTATOR_SHARD_TOTAL']).toBe('2');

    for (const [agent, m] of [
      [a, onA],
      [b, onB],
    ] as const) {
      agent.send({
        type: 'job.state',
        jobId: m.jobId,
        assignmentId: m.assignmentId,
        phase: 'running',
        ts: Date.now(),
      });
    }
    await waitFor(
      async () => (await admin.getGroup(group.id)).state === 'running' || undefined,
      'group running',
    );
    finish(a, onA, 'succeeded');
    finish(b, onB, 'failed');
    const done = await waitFor(async () => {
      const g = await admin.getGroup(group.id);
      return g.state === 'failed' ? g : undefined;
    }, 'group failed');
    expect(done.counts).toEqual({ succeeded: 1, failed: 1 });
    expect(done.nodes).toEqual(['a', 'b']);
    expect(done.finishedAt).not.toBeNull();

    // Retrying the group re-runs only the failed shard, as a new attempt in the same group.
    const retried = await admin.retryGroup(group.id);
    expect(retried.state).toBe('running');
    const again = await waitFor<Extract<ControllerMessage, { type: 'job.assign' }>>(
      () =>
        a.received
          .concat(b.received)
          .find(
            (m): m is Extract<ControllerMessage, { type: 'job.assign' }> => m.type === 'job.assign',
          ),
      'retry assignment',
    );
    const failedShard = done.jobs.find((j) => j.state === 'failed')!;
    expect(again.spec.kind === 'command' && again.spec.command[2]).toBe(
      `run --shard=${failedShard.shardIndex}/2`,
    );
    const g2 = await admin.getGroup(group.id);
    expect(g2.jobs.find((j) => j.shardIndex === failedShard.shardIndex)).toMatchObject({
      attempt: 2,
      retryOf: failedShard.id,
    });
    expect((await admin.listGroups()).map((g) => g.id)).toContain(group.id);
  });

  it('cancels every shard and rejects non-command jobs', async () => {
    const group = await admin.submitGroup({
      spec: { ...SPEC, target: { labels: ['nowhere'] } },
      shards: 3,
    });
    const cancelled = await admin.cancelGroup(group.id);
    expect(cancelled.state).toBe('cancelled');
    expect(cancelled.counts).toEqual({ cancelled: 3 });
    await expect(
      admin.submitGroup({
        spec: { kind: 'docker-build', resources: { cpus: 1, memoryMb: 512 } },
        shards: 2,
      }),
    ).rejects.toMatchObject({ code: 'BAD_REQUEST' });
  });

  it('honours preferred nodes', async () => {
    const a = await addAgent('a');
    const b = await addAgent('b');
    await a.connect();
    await b.connect();
    const job = await admin.submitJob({ ...SPEC, target: { labels: [], prefer: ['b'] } });
    expect((await b.next('job.assign')).jobId).toBe(job.id);
  });
});

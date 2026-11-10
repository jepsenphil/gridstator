#!/usr/bin/env node
// Run after rebuilding the accepted-v1 checkout. No runtime JS dependency is added.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const reference = process.argv[2];
assert(reference, 'Usage: capture-cancel-race.mjs REBUILT_FROZEN_CHECKOUT');
const baseline = JSON.parse(readFileSync(join(root, 'native/contracts/accepted-v1/baseline.json')));
for (const source of baseline.sources) {
  assert.equal(
    `sha256:${createHash('sha256')
      .update(readFileSync(join(reference, source.path)))
      .digest('hex')}`,
    source.sha256,
    `Frozen input changed: ${source.path}`,
  );
}
const { initController, startController } = await import(
  pathToFileURL(join(reference, 'apps/controller/dist/controller.js'))
);
const { GridstatorClient } = await import(
  pathToFileURL(join(reference, 'packages/client/dist/client.js'))
);
const dataDir = await mkdtemp(join(tmpdir(), 'gridstator-v1-cancel-race-'));
let controller;
let admin;
let worker;
let socket;
const messages = [];
async function waitFor(probe) {
  const deadline = Date.now() + 5000;
  for (;;) {
    const value = await probe();
    if (value) return value;
    assert(Date.now() < deadline, 'Frozen race timed out');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
const next = (type) =>
  waitFor(() => {
    const index = messages.findIndex((message) => message.type === type);
    return index < 0 ? undefined : messages.splice(index, 1)[0];
  });
try {
  const initialized = await initController({ dataDir, host: '127.0.0.1', port: 0 });
  controller = await startController({ dataDir, host: '127.0.0.1', port: 0 });
  admin = new GridstatorClient({
    url: controller.url,
    ca: controller.caPem,
    token: initialized.adminToken,
  });
  const info = {
    hostname: 'race-model',
    platform: 'linux',
    arch: 'x64',
    osRelease: 'test',
    cpus: 8,
    memoryTotalMb: 16384,
    agentVersion: '0.1.0',
    docker: { available: true },
    buildx: { available: false },
    act: { available: false },
    executors: ['command'],
  };
  const registered = controller.registerLocalAgent('race-model', info);
  worker = new GridstatorClient({
    url: controller.url,
    ca: controller.caPem,
    token: registered.credential,
  });
  socket = worker.openWebSocket('/v1/agent/ws');
  socket.on('message', (data) => messages.push(JSON.parse(data.toString())));
  await new Promise((resolve, reject) => {
    socket.once('open', resolve);
    socket.once('error', reject);
  });
  const send = (message) => socket.send(JSON.stringify(message));
  send({ type: 'hello', protocolVersion: 1, info, activeJobs: [], results: [] });
  await next('welcome');
  const job = await admin.submitJob({
    kind: 'command',
    command: ['true'],
    resources: { cpus: 1, memoryMb: 256 },
  });
  const assignment = await next('job.assign');
  const identity = { jobId: job.id, assignmentId: assignment.assignmentId };
  send({ type: 'job.state', ...identity, phase: 'running', ts: Date.now() });
  await waitFor(async () => (await admin.getJob(job.id)).state === 'running');
  const first = await admin.cancelJob(job.id);
  const again = await admin.cancelJob(job.id);
  await next('job.cancel');
  send({
    type: 'job.result',
    result: {
      ...identity,
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
  await next('job.ack');
  const finished = await waitFor(async () => {
    const value = await admin.getJob(job.id);
    return value.state === 'succeeded' ? value : undefined;
  });
  const events = await admin.jobEvents(job.id);
  const fixture = {
    referenceRevision: baseline.revision ?? '4b8f13ccf1b21820007fef0ab52143c961b69216',
    scenario: 'A successful result wins while a repeated cancellation request is pending.',
    expected: {
      state: finished.state,
      exitCode: finished.exitCode,
      error: finished.error,
      repeatedCancelPreservesTimestamp: first.cancelRequestedAt === again.cancelRequestedAt,
      cancelRequested: finished.cancelRequestedAt !== null,
      cancelEventCount: events.filter((event) => event.type === 'cancel-requested').length,
    },
  };
  const output = join(root, 'native/contracts/cancel-race-v1.json');
  if (existsSync(output))
    assert.deepEqual(fixture, JSON.parse(readFileSync(output)), 'Frozen race changed');
  else writeFileSync(output, JSON.stringify(fixture, null, 2) + '\n');
  process.stdout.write('Frozen success/pending-cancel race verified over TLS/WebSocket\n');
} finally {
  socket?.terminate();
  worker?.close();
  admin?.close();
  await controller?.close();
  await rm(dataDir, { recursive: true, force: true });
}

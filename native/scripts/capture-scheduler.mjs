#!/usr/bin/env node
// Supplemental oracle: execute only scheduler source whose bytes match the accepted v1 revision.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, existsSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const referenceAt = args.indexOf('--reference-root');
const reference = referenceAt < 0 ? root : resolve(args[referenceAt + 1]);
const baseline = JSON.parse(
  readFileSync(resolve(root, 'native/contracts/accepted-v1/baseline.json')),
);
const sourcePaths = ['packages/scheduler/src/index.ts', 'packages/scheduler/src/index.test.ts'];
const sources = sourcePaths.map((path) => {
  const sha256 = `sha256:${createHash('sha256')
    .update(readFileSync(resolve(reference, path)))
    .digest('hex')}`;
  assert.equal(sha256, baseline.sources.find((source) => source.path === path)?.sha256, path);
  return { path, sha256 };
});
const { schedule } = await import(pathToFileURL(resolve(reference, sourcePaths[0])).href);
const clone = (value) => structuredClone(value);
const node = (id, overrides = {}) => ({
  id,
  name: id,
  online: true,
  info: {
    hostname: 'h',
    platform: 'linux',
    osRelease: '6',
    arch: 'x64',
    cpus: 16,
    memoryTotalMb: 65536,
    agentVersion: '0.1.0',
    docker: { available: true },
    buildx: { available: true },
    act: { available: false },
    executors: ['command'],
  },
  metrics: null,
  settings: {
    maxConcurrency: 4,
    reserved: { cpus: 0, memoryMb: 0 },
    labels: [],
    schedulingMode: 'enabled',
    autoUpdate: 'inherit',
  },
  effectiveLabels: ['os=linux', 'arch=x64', 'docker'],
  allocated: { cpus: 0, memoryMb: 0, jobs: 0 },
  ...overrides,
});
const job = (id, overrides = {}) => ({
  id,
  kind: 'command',
  priority: 0,
  queuedAt: 0,
  resources: { cpus: 2, memoryMb: 4096 },
  target: { labels: [] },
  ...overrides,
});
const settings = (overrides = {}) => ({ ...node('template').settings, ...overrides });
const cases = [];
const add = (id, rows, jobs, nodes, warmNodes = {}) =>
  cases.push({
    id,
    ledgerRows: rows.map((row) => `reference-scheduler-index-${String(row).padStart(3, '0')}`),
    jobs,
    nodes,
    warmNodes,
  });
add('eligible', [1], [job('j')], [node('a')]);
add('no-nodes', [2], [job('j')], []);
add('all-offline', [2], [job('j')], [node('a', { online: false })]);
add(
  'load-score',
  [3],
  [job('j')],
  [node('busy', { allocated: { cpus: 10, memoryMb: 40000, jobs: 3 } }), node('idle')],
);
const laptop = node('laptop', {
  info: { ...node('a').info, cpus: 8, memoryTotalMb: 32768 },
  settings: settings({ reserved: { cpus: 6, memoryMb: 24576 } }),
});
add(
  'host-reservation',
  [4],
  [job('small'), job('big', { resources: { cpus: 4, memoryMb: 4096 } })],
  [laptop],
);
add(
  'same-pass-accounting',
  [5],
  [job('a'), job('b'), job('c')],
  [node('n', { info: { ...node('n').info, cpus: 4 } })],
);
add(
  'concurrency',
  [6],
  [job('a'), job('b')],
  [node('n', { settings: settings({ maxConcurrency: 1 }) })],
);
for (const mode of ['disabled', 'draining'])
  add(mode, [7], [job('j')], [node('d', { settings: settings({ schedulingMode: mode }) })]);
const plain = node('plain');
const gpu = node('gpu', { effectiveLabels: ['docker', 'gpu'] });
add('matching-label', [8], [job('j', { target: { labels: ['gpu'] } })], [plain, gpu]);
add('missing-single-label', [8], [job('j', { target: { labels: ['tpu'] } })], [plain]);
add('missing-all-labels', [8], [job('j', { target: { labels: ['tpu'] } })], [plain, gpu]);
for (const target of ['b', 'b-id', 'missing'])
  add(
    `pin-${target}`,
    [9],
    [job('j', { target: { node: target, labels: [] } })],
    [node('a'), node('b-id', { name: 'b' })],
  );
add(
  'priority-backfill',
  [10],
  [
    job('low'),
    job('huge', { priority: 10, resources: { cpus: 8, memoryMb: 1024 } }),
    job('high', { priority: 5 }),
  ],
  [node('n', { info: { ...node('n').info, cpus: 4 } })],
);
add(
  'fifo',
  [10],
  [job('late', { queuedAt: 2 }), job('early', { queuedAt: 1 })],
  [node('n', { settings: settings({ maxConcurrency: 1 }) })],
);
add(
  'missing-docker',
  [11],
  [job('j')],
  [node('n', { info: { ...node('n').info, docker: { available: false }, executors: [] } })],
);
add('missing-executor', [11], [job('j', { kind: 'actions' })], [node('n')]);
add(
  'live-memory',
  [12],
  [job('j', { resources: { cpus: 1, memoryMb: 2048 } })],
  [
    node('n', {
      metrics: {
        ts: 0,
        cpuPercent: 10,
        memoryAvailableMb: 1024,
        diskFreeMb: 1,
        diskTotalMb: 1,
        load1: 0,
      },
    }),
  ],
);
add('warm-project', [13], [job('j', { project: 'web' })], [node('a'), node('b')], { web: ['b'] });
add(
  'preferred-busy',
  [14],
  [job('j', { target: { labels: [], prefer: ['b'] } })],
  [node('a'), node('b', { allocated: { cpus: 8, memoryMb: 30000, jobs: 2 } })],
);
add(
  'preferred-full-fallback',
  [14],
  [job('j', { target: { labels: [], prefer: ['b'] } })],
  [
    node('a'),
    node('b', {
      settings: settings({ maxConcurrency: 1 }),
      allocated: { cpus: 1, memoryMb: 1, jobs: 1 },
    }),
  ],
);
add(
  'spread-shards',
  [15],
  [1, 2, 3, 4].map((n) => job(`shard${n}`, { group: 'g', resources: { cpus: 1, memoryMb: 1024 } })),
  [node('a'), node('b')],
);
add(
  'running-siblings',
  [15],
  [job('j', { group: 'g' })],
  [node('a', { groups: { g: 1 } }), node('b')],
);
add('rejected-worker', [16], [job('j', { excludedNodes: ['a'] })], [node('a'), node('b')]);
// Interface edge witnesses beyond the original tests: JS UTF-16 string ordering and toFixed ties.
add(
  'utf16-name-tie',
  [],
  [job('j')],
  [node('bmp', { name: '\ue000' }), node('astral', { name: '😀' })],
);
add(
  'pending-round-half',
  [],
  [job('j', { resources: { cpus: 17, memoryMb: 20992 } })],
  [node('n')],
);
add(
  'pending-round-tenth',
  [],
  [job('j', { resources: { cpus: 17, memoryMb: 1280 } })],
  [node('n')],
);
add(
  'pending-tiny-cpu',
  [],
  [job('j', { resources: { cpus: 0.0000001, memoryMb: 4096 } })],
  [node('n', { info: { ...node('n').info, cpus: 0 } })],
);
let seed = 0x47524944;
const next = (count) => {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
  return seed % count;
};
for (let index = 0; index < 32; index++) {
  const nodes = ['a', 'b', 'c'].map((id) =>
    node(id, {
      online: next(5) !== 0,
      info: {
        ...node(id).info,
        cpus: [2, 4, 8, 16][next(4)],
        memoryTotalMb: [4096, 8192, 32768][next(3)],
        executors: next(4) === 0 ? ['build'] : ['command'],
      },
      settings: settings({
        reserved: { cpus: [0, 0.5, 1][next(3)], memoryMb: [0, 1024][next(2)] },
        maxConcurrency: [1, 2, 4][next(3)],
        schedulingMode: ['enabled', 'enabled', 'enabled', 'draining'][next(4)],
      }),
      effectiveLabels: next(2) ? ['docker', 'gpu'] : ['docker'],
      allocated: { cpus: next(4), memoryMb: next(4) * 1024, jobs: next(3) },
      metrics: next(2) ? null : { cpuPercent: next(101), memoryAvailableMb: next(8) * 1024 },
      groups: { g: next(2) },
    }),
  );
  const jobs = Array.from({ length: 5 }, (_, j) =>
    job(`j${j}`, {
      queuedAt: next(4),
      priority: next(5),
      kind: next(5) === 0 ? 'build' : 'command',
      resources: { cpus: [0.5, 1, 2, 4][next(4)], memoryMb: [512, 1024, 4096, 8192][next(4)] },
      target: {
        labels: next(3) === 0 ? ['gpu'] : [],
        prefer: next(2) ? ['b'] : [],
        ...(next(5) === 0 ? { node: ['a', 'b', 'missing'][next(3)] } : {}),
      },
      project: 'web',
      group: 'g',
      excludedNodes: next(3) === 0 ? ['a'] : [],
    }),
  );
  add(`combined-${index}`, [], jobs, nodes, { web: ['c'] });
}
const fixture = {
  formatVersion: 1,
  baselineRevision: baseline.revision,
  sources,
  cases: cases.map((input) => {
    const jobs = clone(input.jobs).map((j) => ({
      ...j,
      excludedNodes: new Set(j.excludedNodes ?? []),
    }));
    const nodes = clone(input.nodes).map((n) => ({
      ...n,
      groups: new Map(Object.entries(n.groups ?? {})),
    }));
    const before = JSON.stringify([jobs, nodes], (_key, value) =>
      value instanceof Set ? [...value] : value instanceof Map ? Object.fromEntries(value) : value,
    );
    const result = schedule(jobs, nodes, {
      warmNodes: (project) => new Set(input.warmNodes[project] ?? []),
    });
    assert.equal(
      JSON.stringify([jobs, nodes], (_key, value) =>
        value instanceof Set
          ? [...value]
          : value instanceof Map
            ? Object.fromEntries(value)
            : value,
      ),
      before,
      input.id,
    );
    return {
      ...input,
      expected: { assignments: result.assignments, pending: Object.fromEntries(result.pending) },
    };
  }),
};
const bytes = JSON.stringify(fixture, null, 2) + '\n';
const output = resolve(root, 'native/contracts/scheduler-v1.json');
if (args.includes('--write')) {
  assert(!existsSync(output), 'refusing to overwrite an existing reference fixture');
  writeFileSync(output, bytes);
} else {
  assert(
    JSON.stringify(JSON.parse(readFileSync(output, 'utf8'))) === JSON.stringify(fixture),
    'frozen scheduler fixture drift',
  );
}
process.stdout.write(
  `Verified ${fixture.cases.length} scheduler scenarios against ${baseline.revision}\n`,
);

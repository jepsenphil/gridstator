#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
const reference = resolve(process.argv[2] ?? '');
const output = process.argv[3];
if (!output) throw new Error('Usage: capture-protocol-boundaries.mjs FROZEN_CHECKOUT OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216')
  throw new Error('Wrong reference revision');
const job = await import(pathToFileURL(join(reference, 'packages/protocol/dist/job.js')));
const common = await import(pathToFileURL(join(reference, 'packages/protocol/dist/common.js')));
const agent = await import(
  pathToFileURL(join(reference, 'packages/protocol/dist/agent-channel.js'))
);
const project = await import(
  pathToFileURL(join(reference, 'packages/protocol/dist/project-config.js'))
);
const schemas = {
  JobSpec: job.JobSpecSchema,
  Timestamp: common.TimestampSchema,
  JobOutputs: job.JobOutputsSchema,
  AgentJobResult: agent.AgentJobResultSchema,
  ProjectConfig: project.ProjectConfigSchema,
};
const digest = `sha256:${'a'.repeat(64)}`;
const bases = {
  command: { kind: 'command', command: ['true'] },
  build: { kind: 'docker-build' },
  actions: { kind: 'actions', workflow: 'ci.yml' },
  outputs: {},
  result: {
    jobId: 'job',
    assignmentId: 'assignment',
    outcome: 'succeeded',
    exitCode: 0,
    error: null,
    finishedAt: 0,
    logSeq: 0,
  },
};
const clone = (value) => structuredClone(value);
function set(input, path, value) {
  let parent = input;
  for (let index = 0; index < path.length - 1; index++) {
    const key = path[index];
    parent = parent[key] ??= typeof path[index + 1] === 'number' ? [] : {};
  }
  parent[path.at(-1)] = value;
}
function compact(value) {
  if (typeof value === 'string' && value.length > 128) {
    const char = Array.from(value)[0];
    if (char.repeat(value.length / char.length) === value)
      return { $text: [char, value.length / char.length] };
  }
  if (Array.isArray(value)) {
    if (
      value.length > 20 &&
      value.every((item) => JSON.stringify(item) === JSON.stringify(value[0]))
    )
      return { $array: [compact(value[0]), value.length] };
    return value.map(compact);
  }
  if (value && typeof value === 'object')
    return Object.fromEntries(Object.entries(value).map(([key, value]) => [key, compact(value)]));
  return value;
}
const cases = [];
function add(id, schema, input) {
  const parsed = schemas[schema].safeParse(input);
  cases.push({
    id,
    schema,
    input: compact(input),
    success: parsed.success,
    ...(parsed.success
      ? { output: compact(parsed.data) }
      : { issues: parsed.error.issues.map((issue) => ({ path: issue.path, code: issue.code })) }),
  });
}
function spec(id, base, path, value) {
  const input = clone(bases[base]);
  set(input, path, value);
  add(id, 'JobSpec', input);
}
function strings(id, base, path, min, max) {
  for (const [suffix, text] of [
    ['empty', ''],
    ['min', 'a'.repeat(min)],
    ['max', 'a'.repeat(max)],
    ['beyond', 'a'.repeat(max + 1)],
    ['unicode-max', '🦀'.repeat(Math.floor(max / 2))],
    ['unicode-beyond', '🦀'.repeat(Math.floor(max / 2) + 1)],
    ['unicode-codepoint-max', '🦀'.repeat(max)],
    ['unicode-codepoint-beyond', '🦀'.repeat(max + 1)],
  ])
    spec(`${id}-${suffix}`, base, path, text);
  spec(`${id}-null`, base, path, null);
}
function arrays(id, base, path, value, max) {
  for (const length of [0, 1, max, max + 1])
    spec(
      `${id}-${length}`,
      base,
      path,
      Array.from({ length }, () => clone(value)),
    );
  spec(`${id}-null`, base, path, null);
}
for (const base of ['command', 'build', 'actions']) {
  strings(`${base}-name`, base, ['name'], 1, 200);
  strings(`${base}-project`, base, ['project'], 1, 200);
  for (const [id, path, values] of [
    ['cpus', ['resources'], [0, -1, Number.MIN_VALUE, 0.125, 1024, 1024.125]],
    ['memory', ['resources'], [15, 16, 16.5, 16777216, 16777217]],
  ])
    for (const value of values)
      spec(
        `${base}-${id}-${value}`,
        base,
        path,
        id === 'cpus' ? { cpus: value, memoryMb: 16 } : { cpus: 1, memoryMb: value },
      );
  for (const [name, min, max] of [
    ['priority', -100, 100],
    ['timeoutSeconds', 1, 604800],
  ])
    for (const value of [min - 1, min, min + 0.5, max, max + 1, null])
      spec(`${base}-${name}-${value}`, base, [name], value);
  arrays(`${base}-labels`, base, ['target', 'labels'], 'a', 32);
  arrays(`${base}-prefer`, base, ['target', 'prefer'], 'a', 16);
  strings(`${base}-target-node`, base, ['target', 'node'], 1, 128);
  for (const value of [-1, 0, 0.5, 10, 11, null])
    spec(`${base}-retry-max-${value}`, base, ['retry', 'max'], value);
  for (const field of ['env', 'secretEnv']) {
    for (const value of ['', 'x'.repeat(128 * 1024), 'x'.repeat(128 * 1024 + 1)])
      spec(`${base}-${field}-${value.length}`, base, [field], { TOKEN: value });
    for (const name of ['A', '_A', '1A', 'A-B', 'A\n', 'A\r\n'])
      spec(`${base}-${field}-key-${JSON.stringify(name)}`, base, [field], { [name]: 'value' });
  }
  for (const field of ['env', 'secretEnv', 'resources', 'target', 'retry', 'workdir', 'snapshot'])
    spec(`${base}-${field}-null`, base, [field], null);
  for (const workdir of ['.', '', 'a/../b', 'C:relative', 'a\u0000b', 'a\n'])
    spec(`${base}-workdir-${JSON.stringify(workdir)}`, base, ['workdir'], workdir);
  spec(`${base}-unknown-fields`, base, ['unknown'], true);
}
strings('image', 'command', ['image'], 1, 512);
arrays('command', 'command', ['command'], 'x', 4096);
strings('argv-entry', 'command', ['command', 0], 0, 256 * 1024);
arrays('artifacts', 'command', ['artifacts'], 'a', 32);
for (const base of ['command', 'build', 'actions'])
  for (const value of [digest, `sha256:${'A'.repeat(64)}`, `sha256:${'a'.repeat(63)}`, ''])
    spec(`${base}-snapshot-${value}`, base, ['snapshot'], value);
for (const [base, field] of [
  ['command', 'artifacts'],
  ['build', 'context'],
  ['build', 'dockerfile'],
  ['actions', 'workflow'],
])
  for (const value of [
    '.',
    '',
    'C:relative',
    'a\u0000b',
    'a\nb',
    'a'.repeat(1024),
    'a'.repeat(1025),
  ])
    spec(
      `${base}-${field}-${JSON.stringify(value).slice(0, 40)}`,
      base,
      [field],
      field === 'artifacts' ? [value] : value,
    );
strings('build-stage', 'build', ['stage'], 1, 128);
strings('actions-job', 'actions', ['job'], 1, 200);
for (const [field, max, length] of [
  ['tags', 32, 512],
  ['platforms', 8, 64],
  ['cacheFrom', 8, 512],
  ['cacheTo', 8, 512],
]) {
  arrays(`build-${field}`, 'build', [field], 'a', max);
  strings(`build-${field}-entry`, 'build', [field, 0], 1, length);
}
for (const [field, maxkey, minval, maxval] of [
  ['inputs', 200, 0, 65536],
  ['platforms', 100, 1, 512],
]) {
  for (const n of [0, 1, maxkey, maxkey + 1])
    spec(`actions-${field}-key-${n}`, 'actions', [field], { ['a'.repeat(n)]: 'x' });
  for (const n of [minval, maxval, maxval + 1])
    spec(`actions-${field}-value-${n}`, 'actions', [field], { a: 'x'.repeat(n) });
}
for (const n of [0, 65536, 65537])
  spec(`build-args-value-${n}`, 'build', ['buildArgs'], { ARG: 'x'.repeat(n) });
for (const name of ['ARG', 'a.b-c', '1arg', 'A\n'])
  spec(`build-args-key-${JSON.stringify(name)}`, 'build', ['buildArgs'], { [name]: 'x' });
arrays('actions-matrix', 'actions', ['matrix'], 'node:20', 32);
for (const text of [
  'node:20',
  'node:20\n',
  'node\n:20',
  ':20',
  'node:',
  'node:20\r',
  'node:20\u2028',
  'a'.repeat(198) + ':1',
  'a'.repeat(199) + ':1',
])
  spec(`actions-matrix-${JSON.stringify(text).slice(0, 50)}`, 'actions', ['matrix'], [text]);
for (const event of ['push', 'workflow_dispatch', '', 'PUSH', 'push\n', 'push-branch'])
  spec(`actions-event-${JSON.stringify(event)}`, 'actions', ['event'], event);
for (const [field, max] of [
  ['branch', 255],
  ['remoteUrl', 1024],
  ['headSha', 64],
])
  strings(`actions-git-${field}`, 'actions', ['git', field], 0, max);
for (const value of [{}, null, [], true, { a: [1, null, {}, false] }])
  spec(`actions-payload-${JSON.stringify(value)}`, 'actions', ['eventPayload'], value);
for (const value of [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1, null])
  add(`timestamp-${value}`, 'Timestamp', value);
const step = {
  id: '',
  name: '',
  group: null,
  status: 'success',
  startedAt: null,
  durationMs: null,
};
for (const [field, max] of [
  ['id', 300],
  ['name', 1000],
  ['group', 300],
])
  for (const n of [0, max, max + 1]) {
    const s = { ...step, [field]: 'a'.repeat(n) };
    add(`step-${field}-${n}`, 'JobOutputs', { steps: [s] });
  }
for (const field of Object.keys(step)) {
  const s = { ...step };
  delete s[field];
  add(`step-missing-${field}`, 'JobOutputs', { steps: [s] });
}
for (const value of [-1, 0, 1.25, null, '1'])
  add(`step-duration-${value}`, 'JobOutputs', { steps: [{ ...step, durationMs: value }] });
for (const value of [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1])
  add(`step-started-${value}`, 'JobOutputs', { steps: [{ ...step, startedAt: value }] });
for (const n of [0, 5000, 5001])
  add(`steps-count-${n}`, 'JobOutputs', { steps: Array.from({ length: n }, () => step) });
const image = { id: null, digest: null, tags: [], sizeBytes: null, output: 'load' };
for (const field of Object.keys(image)) {
  const value = { ...image };
  delete value[field];
  add(`image-missing-${field}`, 'JobOutputs', { image: value });
}
for (const value of [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1, null])
  add(`image-size-${value}`, 'JobOutputs', { image: { ...image, sizeBytes: value } });
add('outputs-unknown-strip', 'JobOutputs', {
  unknown: true,
  image: { ...image, unknown: true },
  steps: [{ ...step, unknown: true }],
});
for (const field of Object.keys(bases.result)) {
  const input = clone(bases.result);
  delete input[field];
  add(`result-missing-${field}`, 'AgentJobResult', input);
}
for (const [field, values] of [
  ['logSeq', [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1]],
  ['finishedAt', [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1]],
  ['exitCode', [null, -1, 0, 255, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1]],
  ['error', [null, '', 'a'.repeat(8192), 'a'.repeat(8193)]],
])
  for (const value of values)
    add(`result-${field}-${typeof value === 'string' ? value.length : value}`, 'AgentJobResult', {
      ...bases.result,
      [field]: value,
    });
const artifact = { name: 'a', digest, size: 0 };
for (const n of [0, 1000, 1001])
  add(`result-artifacts-${n}`, 'AgentJobResult', {
    ...bases.result,
    artifacts: Array.from({ length: n }, () => artifact),
  });
for (const size of [-1, 0, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1])
  add(`artifact-size-${size}`, 'AgentJobResult', {
    ...bases.result,
    artifacts: [{ ...artifact, size }],
  });
for (const n of [0, 1024, 1025])
  add(`artifact-name-${n}`, 'AgentJobResult', {
    ...bases.result,
    artifacts: [{ ...artifact, name: 'a'.repeat(n) }],
  });
for (const field of ['outputs', 'oomKilled', 'artifacts'])
  add(`result-${field}-null`, 'AgentJobResult', { ...bases.result, [field]: null });
for (const field of ['env', 'secretEnv'])
  for (const length of [128 * 1024, 128 * 1024 + 1])
    spec(`command-${field}-unicode-${length}`, 'command', [field], { VALUE: '🦀'.repeat(length) });
for (const field of ['name', 'project', 'image'])
  spec(`command-${field}-null-required`, 'command', [field], null);
for (const [field, min, max] of [
  ['priority', -100, 100],
  ['shards', 1, 64],
])
  for (const value of [min - 1, min, min + 0.5, max, max + 1, null])
    add(`project-task-${field}-${value}`, 'ProjectConfig', {
      tasks: { test: { command: 'true', [field]: value } },
    });
for (const value of [0, 1, 1.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1, null, ''])
  add(`project-size-${value}`, 'ProjectConfig', { snapshot: { maxFileSize: value } });
for (const length of [128 * 1024, 128 * 1024 + 1])
  add(`project-env-unicode-${length}`, 'ProjectConfig', {
    tasks: { test: { command: 'true', env: { VALUE: '🦀'.repeat(length) } } },
  });
for (const [field, values] of [
  [
    'resources',
    [
      {},
      null,
      { cpus: Number.MIN_VALUE },
      { cpus: 0 },
      { memory: 1 },
      { memory: 0 },
      { memory: 1.5 },
    ],
  ],
  ['retry', [{}, null, { max: 10 }, { max: 11 }, { on: [] }, { on: ['invalid'] }]],
  ['command', ['', [], [''], ['true'], null]],
  ['env', [{}, null, { INVALID_NAME: 'x' }, { 'bad-name': 'x' }]],
])
  for (let index = 0; index < values.length; index++)
    add(`project-task-${field}-shape-${index}`, 'ProjectConfig', {
      tasks: { test: { command: 'true', [field]: values[index] } },
    });
for (const field of ['defaults', 'snapshot', 'tasks', 'builds', 'actions'])
  add(`project-${field}-null`, 'ProjectConfig', { [field]: null });
writeFileSync(
  output,
  JSON.stringify(
    {
      sourceRevision: revision,
      encoding:
        'Objects with exactly $text:[text,count] or $array:[value,count] expand repeated strings/arrays; all other objects are ordinary JSON.',
      schemas: Object.keys(schemas),
      cases,
    },
    null,
    2,
  ) + '\n',
);
process.stdout.write(`Captured ${cases.length} protocol boundary cases\n`);

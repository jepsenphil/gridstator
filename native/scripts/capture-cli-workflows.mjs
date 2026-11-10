#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
const reference = resolve(process.argv[2] ?? '');
const output = process.argv[3];
if (!output) throw new Error('Usage: capture-cli-workflows.mjs FROZEN_CHECKOUT OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216') throw new Error('Wrong reference');
const { Command } = await import(
  pathToFileURL(join(reference, 'apps/cli/node_modules/commander/index.js'))
);
const { registerClientCommands, buildSpec } = await import(
  pathToFileURL(join(reference, 'apps/cli/dist/commands/client.js'))
);
const { JobSpecSchema } = await import(
  pathToFileURL(join(reference, 'packages/protocol/dist/job.js'))
);
const { ProjectConfigSchema } = await import(
  pathToFileURL(join(reference, 'packages/protocol/dist/project-config.js'))
);
const { parseBytes, parseMegabytes, parseDuration } = await import(
  pathToFileURL(join(reference, 'packages/shared/dist/format.js'))
);
const { settingsFromFlags } = await import(
  pathToFileURL(join(reference, 'apps/cli/dist/settings-flags.js'))
);
const { exitCodeForJob } = await import(pathToFileURL(join(reference, 'apps/cli/dist/output.js')));
const cases = [];
const task = {
  command: 'npm test',
  image: 'task-image',
  env: { FROM_TASK: 'task', OVERRIDE: 'task' },
  secretEnv: ['CLI_FIXTURE_SECRET'],
  resources: { cpus: 0.5, memory: '512m' },
  workdir: './packages/test',
  labels: ['task', 'shared'],
  prefer: ['task-node', 'shared'],
  priority: 3,
  retry: { max: 2, on: ['failed'] },
  timeout: '30s',
  artifacts: ['output/report'],
  junit: 'output/junit.xml',
  node: 'task-node',
};
const config = {
  defaults: {
    image: 'default-image',
    resources: { cpus: 3, memory: '2g' },
    labels: ['default', 'shared'],
    prefer: ['default-node', 'shared'],
    timeout: '1m',
  },
  tasks: { test: task },
};
function run(id, argv, raw = {}, workdir = '.', extras = {}) {
  const environment = { CLI_FIXTURE_SECRET: 'fixture-secret', CLI_FIXTURE_ENV: 'from-shell' };
  Object.assign(process.env, environment);
  const input = { id, argv: ['run', ...argv], config: raw, workdir, environment, ...extras };
  try {
    const program = new Command().exitOverride();
    registerClientCommands(program);
    const command = program.commands.find((command) => command.name() === 'run');
    // Parse through the root as the real CLI does, including deferred unknown
    // child flags. Replacing only the action prevents network/capture side effects.
    command.action(() => {});
    program.parse(['node', 'gridstator', 'run', ...argv]);
    const flags = command.opts();
    const project = ProjectConfigSchema.safeParse(raw);
    if (!project.success) throw Object.assign(new Error('invalid project'), { code: 'CONFIG' });
    const built = buildSpec(command.processedArgs[0], flags, project.data, {
      workdir,
      project: project.data.project ?? 'project-fixture',
      ...(extras.snapshot ? { snapshot: extras.snapshot } : {}),
    });
    const spec = JobSpecSchema.safeParse(built);
    if (!spec.success) throw Object.assign(new Error('invalid job spec'), { code: 'BAD_REQUEST' });
    const shards =
      flags.shards !== undefined
        ? Number(flags.shards)
        : ((flags.task ? project.data.tasks[flags.task]?.shards : undefined) ?? 1);
    if (!Number.isInteger(shards) || shards < 1 || shards > 64)
      throw Object.assign(new Error('invalid shards'), { code: 'BAD_REQUEST' });
    cases.push({ ...input, success: true, output: spec.data, shards });
  } catch (error) {
    cases.push({
      ...input,
      success: false,
      exitCode: error.code === 'BAD_REQUEST' || error.code === 'CONFIG' ? 2 : 1,
      message: error.message,
    });
  }
}
run('argv', ['--', 'echo', 'hello']);
run('shell-options-after-deferred-flags', [
  '--no-snapshot',
  '--image',
  'alpine:3.20',
  '--node',
  'beta',
  '--',
  'sh',
  '-c',
  'printf literal',
]);
run('literal-help-after-deferred-flags', [
  '--image',
  'alpine:3.20',
  '--',
  'echo',
  '--help',
  '-h',
  '--version',
]);
run('single-shell', ['--', 'echo hello']);
run('forced-shell', ['--shell', '--', 'echo', 'hello']);
run('options-after-argv', ['echo', 'hello', '--image', 'alpine:3.20']);
run('repeat-scalar-last', ['--image', 'first', '--image', 'last', '--', 'true']);
run('repeat-env-last', [
  '-e',
  'A=first',
  '--env',
  'A=last',
  '--env',
  'CLI_FIXTURE_ENV',
  '--',
  'true',
]);
run('task-defaults', ['--task', 'test'], config);
run('task-command-override', ['--task', 'test', '--', 'printf', 'override'], config);
run(
  'flags-override-task',
  [
    '--task',
    'test',
    '--image',
    'flag-image',
    '--cpus',
    '0.25',
    '--memory',
    '1.5g',
    '--priority',
    '-10',
    '--retries',
    '3',
    '--timeout',
    '1h30m',
    '--node',
    'flag-node',
    '--label',
    ' shared, extra , ,last',
    '--prefer',
    'flag-node',
    '--env',
    'OVERRIDE=flag',
    '--artifact',
    './more',
  ],
  config,
  'packages/web',
);
run('artifact-relative', ['--artifact', '../reports/../result', '--', 'true'], {}, 'packages/web');
run('artifact-absolute-looking', ['--artifact', '/report', '--', 'true']);
run('snapshot-identity', ['--', 'true'], {}, '.', { snapshot: `sha256:${'a'.repeat(64)}` });
run('empty-task-clears', ['--task', '', '--', 'true'], config);
run('empty-name-clears', ['--name', '', '--', 'true']);
run('empty-name-task-fallback', ['--name', '', '--task', 'test'], config);
run('empty-node-clears', ['--task', 'test', '--node', ''], config);
run('task-hidden-workdir', ['--task', 'test'], {
  tasks: { test: { command: 'true', workdir: '.config' } },
});
run('task-absolute-workdir', ['--task', 'test'], {
  tasks: { test: { command: 'true', workdir: '/absolute' } },
});
run('missing-task', ['--task', 'missing'], config);
run('missing-command', []);
run('invalid-label', ['--label', 'bad label', '--', 'true']);
run('invalid-shards', ['--shards', '65', '--', 'true']);
for (const value of [' 2 ', '0x2', '0o2', '0b10', '2e0', '\ufeff2\ufeff'])
  run(`numeric-${JSON.stringify(value)}`, [
    '--cpus',
    value,
    '--priority',
    value,
    '--retries',
    value,
    '--',
    'true',
  ]);
run('empty-numeric-zero', ['--priority', '', '--retries', '', '--', 'true']);
run('invalid-number', ['--cpus', 'nope', '--', 'true']);
run('invalid-memory', ['--memory', 'invalid', '--', 'true']);
run('invalid-timeout', ['--timeout', 'invalid', '--', 'true']);
run('fractional-resources', ['--task', 'test'], {
  tasks: {
    test: { command: 'true', resources: { cpus: 0.125, memory: 16 }, priority: -100, shards: 64 },
  },
});
const units = [];
for (const [name, fn, inputs] of [
  [
    'bytes',
    parseBytes,
    [
      '0',
      '0.5',
      '1.5GiB',
      '512m',
      '1KiB',
      '1 MB',
      '\ufeff512m\ufeff',
      '\u0085512m',
      '-1',
      '1e3',
      '1unknown',
    ],
  ],
  [
    'megabytes',
    parseMegabytes,
    ['512', ' 512 ', '1g', '1.5g', '0.5', '\ufeff512\ufeff', '\u0085512', 'invalid'],
  ],
  [
    'duration',
    parseDuration,
    [
      '1h30m',
      '0.5s',
      '1ms',
      '1d',
      ' 1s ',
      '\ufeff1s\ufeff',
      '\u00851s',
      '1s 2m',
      '1.',
      '-1s',
      '1e3s',
      'invalid',
    ],
  ],
])
  for (const input of inputs) {
    try {
      units.push({ fn: name, input, success: true, output: fn(input) });
    } catch {
      units.push({ fn: name, input, success: false });
    }
  }
const settings = [];
for (const value of [' 1 ', '0x2', '0o2', '0b10', '\ufeff2\ufeff', '-1', '1.5', 'invalid'])
  for (const key of ['reserveCpus', 'maxConcurrency']) {
    try {
      settings.push({
        flags: { [key]: value },
        success: true,
        output: settingsFromFlags({ [key]: value }),
      });
    } catch {
      settings.push({ flags: { [key]: value }, success: false });
    }
  }
for (const labels of ['fast, ssd', 'fast, ,ssd', '\ufefffast\ufeff,ssd', 'bad label']) {
  try {
    settings.push({ flags: { labels }, success: true, output: settingsFromFlags({ labels }) });
  } catch {
    settings.push({ flags: { labels }, success: false });
  }
}
const exits = [];
for (const state of [
  'created',
  'queued',
  'assigned',
  'starting',
  'running',
  'succeeded',
  'failed',
  'cancelled',
  'lost',
])
  for (const exitCode of [null, -1, 0, 1, 7, 75, 130, 255, 256])
    exits.push({ state, exitCode, output: exitCodeForJob({ state, exitCode }) });
writeFileSync(
  output,
  JSON.stringify({ sourceRevision: revision, cases, units, settings, exits }, null, 2) + '\n',
);
process.stdout.write(
  `Captured ${cases.length} source workflow specs,${units.length} unit cases,${settings.length} settings cases,${exits.length} exits\n`,
);

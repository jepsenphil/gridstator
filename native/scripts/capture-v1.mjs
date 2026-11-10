#!/usr/bin/env node
// Freeze new references in a new directory; accepted-v1 is never overwritten.
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { hostname } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const accepted = join(root, 'native/contracts/accepted-v1');
const args = process.argv.slice(2);
const checking = args.length === 0 || (args.length === 1 && args[0] === '--check');
if (!checking && (args.length !== 2 || args[0] !== '--output')) {
  throw new Error('Usage: node native/scripts/capture-v1.mjs [--check | --output <new-directory>]');
}
const destination = checking ? accepted : resolve(root, args[1]);
if (!checking && existsSync(destination))
  throw new Error('Reference directories are immutable; choose a new directory');
const digest = (bytes) => `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
const git = (...options) => execFileSync('git', options, { cwd: root, encoding: 'utf8' }).trim();
const baseline = checking
  ? JSON.parse(readFileSync(join(accepted, 'baseline.json'), 'utf8'))
  : undefined;
const revision = baseline?.revision ?? git('rev-parse', 'HEAD');
const sourcePaths = git('ls-tree', '-r', '--name-only', revision)
  .split('\n')
  .filter(
    (path) =>
      /^(apps|packages|scripts|tests)\//.test(path) ||
      /^(package\.json|pnpm-lock\.yaml|tsconfig.*\.json)$/.test(path),
  );
const currentPaths = git(
  'ls-files',
  '--cached',
  '--others',
  '--exclude-standard',
  'apps',
  'packages',
  'scripts',
  'tests',
  'package.json',
  'pnpm-lock.yaml',
  'tsconfig*.json',
).split('\n');
const addedSources = currentPaths.filter((path) => !sourcePaths.includes(path));
if (addedSources.length)
  throw new Error(`Additional reference inputs require review: ${addedSources.join(', ')}`);
const sources = sourcePaths.map((path) => {
  const bytes = execFileSync('git', ['show', `${revision}:${path}`], { cwd: root });
  if (!existsSync(join(root, path)) || digest(readFileSync(join(root, path))) !== digest(bytes)) {
    throw new Error(
      `Reference source changed: ${path}. Check out the frozen revision to reproduce; review later v1 changes separately.`,
    );
  }
  return { path, sha256: digest(bytes) };
});
// Build from checked source, never trust existing ignored dist files.
execFileSync('pnpm', ['exec', 'tsc', '-b', '--force', '--pretty', 'false'], {
  cwd: root,
  stdio: 'inherit',
});
const moduleAt = (path) => import(pathToFileURL(join(root, path)).href);
const referencePreload = join(root, 'native/scripts/reference-hostname.cjs');
await moduleAt('native/scripts/reference-hostname.cjs');
const manifest = await moduleAt('packages/protocol/dist/manifest.js');
const states = await moduleAt('packages/protocol/dist/states.js');
const job = await moduleAt('packages/protocol/dist/job.js');
const common = await moduleAt('packages/protocol/dist/common.js');
const config = await moduleAt('packages/protocol/dist/project-config.js');
const errors = await moduleAt('packages/shared/dist/errors.js');
const format = await moduleAt('packages/shared/dist/format.js');
const files = {};
const file = (name, body) => {
  files[name] = JSON.stringify(body, null, 2) + '\n';
};
const entry = (path, executable = false) => ({
  path,
  type: 'file',
  size: 0,
  digest: digest(''),
  executable,
});
const manifestInputs = [
  ['empty', []],
  [
    'ascii-files-and-link',
    [
      entry('README.md'),
      entry('bin/run', true),
      { path: 'link', type: 'symlink', target: 'bin/run' },
    ],
  ],
  [
    'unicode-utf8-order',
    ['a', 'é.txt', '\uE000.txt', '💾.txt', '𐀀.txt']
      .map((path) => entry(path))
      .sort((a, b) => manifest.comparePaths(a.path, b.path)),
  ],
  [
    'json-escaping',
    [
      entry('control\t\n\r\b\f\u0001.txt'),
      entry('html<&>".txt'),
      entry('line\u2028paragraph\u2029.txt'),
    ],
  ],
  [
    'link-parent-stays-inside',
    [{ path: 'dir/link', type: 'symlink', target: '../target' }, entry('target')],
  ],
];
file('manifests.json', {
  formatVersion: 1,
  canonicalEncoding: 'UTF-8, fixed key order, JSON.stringify escaping, no BOM/newline',
  cases: manifestInputs.map(([id, entries]) => {
    const input = { version: 1, entries };
    manifest.validateManifest(input);
    const canonical = manifest.serializeManifest(input);
    return {
      id,
      input,
      canonical,
      canonicalUtf8Hex: Buffer.from(canonical).toString('hex'),
      sha256: digest(canonical),
    };
  }),
  invalid: [
    ['empty-path', [entry('')]],
    ['parent', [entry('../escape')]],
    ['absolute', [entry('/escape')]],
    ['drive', [entry('C:escape')]],
    ['backslash', [entry('dir\\file')]],
    ['nul', [entry('bad\0file')]],
    ['dot-segment', [entry('dir/./file')]],
    ['empty-segment', [entry('dir//file')]],
    ['duplicates', [entry('a'), entry('a')]],
    ['unsorted', [entry('b'), entry('a')]],
    ['nested-file', [entry('a'), entry('a/b')]],
    ['escaping-link', [{ path: 'link', type: 'symlink', target: '../outside' }]],
    ['nested-link', [{ path: 'a', type: 'symlink', target: 'x' }, entry('a/b')]],
  ].map(([id, entries]) => {
    const input = { version: 1, entries };
    try {
      manifest.parseManifest(JSON.stringify(input));
      throw new Error(`Expected rejection: ${id}`);
    } catch (error) {
      if (error.message.startsWith('Expected rejection:')) throw error;
      return { id, input, error: error.message };
    }
  }),
});
file('job-transitions.json', {
  states: states.JOB_STATES,
  terminal: states.JOB_STATES.filter(states.isTerminal),
  dispatched: [...states.DISPATCHED_STATES],
  cases: states.JOB_STATES.flatMap((from) =>
    states.JOB_STATES.map((to) => ({ from, to, allowed: states.canTransition(from, to) })),
  ),
});
const schemaCases = [];
const sample = (schemaName, id, schema, input) => {
  const result = schema.safeParse(input);
  schemaCases.push({
    id,
    schema: schemaName,
    input,
    success: result.success,
    ...(result.success
      ? { output: result.data }
      : {
          issues: result.error.issues.map(({ code, path, message }) => ({ code, path, message })),
        }),
  });
};
const command = { kind: 'command', command: ['echo', 'ok'] };
for (const [id, input] of [
  ['command-defaults', command],
  ['build-defaults', { kind: 'docker-build' }],
  ['actions-defaults', { kind: 'actions', workflow: '.github/workflows/test.yml' }],
  ['unknown-field-stripped', { ...command, unknown: 'strip-me', target: { extra: true } }],
  ['empty-option-objects', { ...command, target: {}, retry: {} }],
  ['null-env', { ...command, env: null }],
  ['null-name', { ...command, name: null }],
  ['empty-command', { ...command, command: [] }],
  ['empty-argv-item-accepted', { ...command, command: [''] }],
  ['fractional-cpu', { ...command, resources: { cpus: 0.25, memoryMb: 16 } }],
  ['maximum-resources', { ...command, resources: { cpus: 1024, memoryMb: 16777216 } }],
  ['zero-cpus', { ...command, resources: { cpus: 0, memoryMb: 4096 } }],
  ['over-cpu-limit', { ...command, resources: { cpus: 1025, memoryMb: 4096 } }],
  ['below-memory-minimum', { ...command, resources: { cpus: 2, memoryMb: 15 } }],
  ['fractional-memory', { ...command, resources: { cpus: 2, memoryMb: 16.5 } }],
  ['over-memory-limit', { ...command, resources: { cpus: 2, memoryMb: 16777217 } }],
  ['resources-required-members', { ...command, resources: {} }],
  ['priority-minimum', { ...command, priority: -100 }],
  ['priority-maximum', { ...command, priority: 100 }],
  ['priority-underflow', { ...command, priority: -101 }],
  ['priority-overflow', { ...command, priority: 101 }],
  ['timeout-minimum', { ...command, timeoutSeconds: 1 }],
  ['timeout-maximum', { ...command, timeoutSeconds: 604800 }],
  ['timeout-zero', { ...command, timeoutSeconds: 0 }],
  ['timeout-overflow', { ...command, timeoutSeconds: 604801 }],
  ['retry-maximum', { ...command, retry: { max: 10, on: ['failed', 'lost'] } }],
  ['retry-overflow', { ...command, retry: { max: 11 } }],
  ['retry-empty-on', { ...command, retry: { on: [] } }],
  ['invalid-env-key', { ...command, env: { 'INVALID-NAME': 'x' } }],
  ['workdir-parent', { ...command, workdir: '../outside' }],
  ['workdir-drive-v1-accepted', { ...command, workdir: 'C:relative' }],
  ['artifact-drive-rejected', { ...command, artifacts: ['C:relative'] }],
  ['invalid-kind', { kind: 'other' }],
])
  sample('JobSpec', id, job.JobSpecSchema, input);
for (const [id, input] of [
  ['project-defaults', {}],
  ['project-unknown-field', { extra: true }],
  ['project-null-defaults', { defaults: null }],
  ['project-empty-snapshot', { snapshot: {} }],
  ['project-partial-retry', { tasks: { test: { command: 'true', retry: {} } } }],
  ['project-nested-unknown', { defaults: { extra: true } }],
])
  sample('ProjectConfig', id, config.ProjectConfigSchema, input);
for (const [id, input] of [
  ['timestamp-zero', 0],
  ['timestamp-milliseconds', 1700000000123],
  ['timestamp-negative', -1],
  ['timestamp-fractional', 1.5],
]) {
  sample('Timestamp', id, common.TimestampSchema, input);
}
for (const [id, input] of [
  ['duration-negative-clamped', -1],
  ['duration-null', null],
]) {
  sample('JobStep', id, job.JobStepSchema, {
    id: 'step',
    group: null,
    name: 'Step',
    status: 'success',
    startedAt: null,
    durationMs: input,
  });
}
file('schema-cases.json', {
  notes:
    'Outputs preserve omission/null, defaults, unknown-field and unit distinctions. Zod issue wording is diagnostic evidence, not a new Rust API requirement.',
  cases: schemaCases,
});
file('errors-and-units.json', {
  errors: errors.ERROR_CODES,
  bodies: [
    new errors.GridstatorError('BAD_REQUEST', 'invalid input'),
    new errors.GridstatorError('NOT_FOUND', 'missing', {
      hint: 'check id',
      details: { id: 'example' },
    }),
  ].map((error) => ({ body: error.toBody(), status: error.status, exitCode: error.exitCode })),
  units: [
    ['parseBytes', '1.5GiB'],
    ['parseBytes', '512m'],
    ['parseBytes', '0.5'],
    ['parseMegabytes', '512'],
    ['parseMegabytes', '1g'],
    ['parseDuration', '1h30m'],
    ['parseDuration', '0.5s'],
  ].map(([fn, input]) => ({ fn, input, output: format[fn](input) })),
});
// Registration has no actions. Only process invocations ending in --help are executed.
const env = {
  ...process.env,
  NO_COLOR: '1',
  FORCE_COLOR: '0',
  GRIDSTATOR_CONFIG: '/V1_CONFIG/client.json',
  GRIDSTATOR_CONTROLLER_DATA: '/V1_DATA/controller',
  GRIDSTATOR_AGENT_DATA: '/V1_DATA/agent',
};
Object.assign(process.env, env);
const { Command } = await import(
  pathToFileURL(join(root, 'apps/cli/node_modules/commander/index.js')).href
);
const program = new Command()
  .name('gridstator')
  .description(
    'Gridstator: multiple machines, one workflow. Run builds and tests on your LAN cluster.',
  )
  .version('0.1.0', '-v, --version');
for (const [path, name] of [
  ['client', 'registerClientCommands'],
  ['agents', 'registerAgentsCommands'],
  ['workloads', 'registerWorkloadCommands'],
  ['doctor', 'registerDoctorCommand'],
  ['server', 'registerServerCommands'],
  ['service', 'registerServiceCommands'],
]) {
  const register = await moduleAt(`apps/cli/dist/commands/${path}.js`);
  register[name](program);
}
const nodeName =
  hostname()
    .split('.')[0]
    .replace(/[^A-Za-z0-9._-]/g, '-')
    .replace(/^[^A-Za-z0-9]+/, '') || 'node';
const normalize = (value) =>
  typeof value === 'string' ? value.replaceAll(nodeName, '<HOSTNAME>') : value;
const commandCases = [];
function commands(current, path = []) {
  const result = spawnSync(
    process.execPath,
    ['--require', referencePreload, join(root, 'apps/cli/dist/main.js'), ...path, '--help'],
    { cwd: root, env, encoding: 'utf8', timeout: 15000 },
  );
  if (result.error || result.status !== 0)
    throw new Error(`CLI help failed (${path.join(' ')}): ${result.error ?? result.stderr}`);
  commandCases.push({
    path,
    aliases: current.aliases(),
    description: current.description(),
    options: current.options.map((option) => ({
      flags: option.flags,
      description: option.description,
      ...(option.defaultValue === undefined ? {} : { default: normalize(option.defaultValue) }),
    })),
    exitCode: result.status,
    stdout: normalize(result.stdout),
    stderr: result.stderr,
  });
  for (const child of current.commands) commands(child, [...path, child.name()]);
}
commands(program);
file('cli-help.json', {
  normalization: {
    hostname: '<HOSTNAME>',
    controllerDataDir: '/V1_DATA/controller',
    agentDataDir: '/V1_DATA/agent',
    clientConfig: '/V1_CONFIG/client.json',
    terminal: 'non-TTY NO_COLOR',
  },
  cases: commandCases,
});
const errorsAndVersion = [
  ['--version'],
  ['__unknown_command__'],
  ['run', '--__unknown_flag__'],
  ['login'],
].map((argv) => {
  const result = spawnSync(
    process.execPath,
    ['--require', referencePreload, join(root, 'apps/cli/dist/main.js'), ...argv],
    {
      cwd: root,
      env,
      encoding: 'utf8',
      timeout: 15000,
    },
  );
  if (result.error || result.status === null)
    throw new Error(`CLI fixture failed: ${argv.join(' ')}`);
  return {
    argv,
    exitCode: result.status,
    stdout: normalize(result.stdout),
    stderr: normalize(result.stderr),
  };
});
file('cli-process.json', { cases: errorsAndVersion });
// Keep the outer fixture containers compatible with the repository formatter. Canonical manifest
// strings and their hashes remain generated solely by the reference implementation above.
const prettier = await import(pathToFileURL(join(root, 'node_modules/prettier/index.mjs')).href);
const formatting = {
  ...(await prettier.resolveConfig(join(root, 'package.json'))),
  parser: 'json',
};
for (const [name, bytes] of Object.entries(files))
  files[name] = await prettier.format(bytes, formatting);
const metadata = baseline ?? {
  formatVersion: 1,
  generation: 'accepted-v1',
  recordedDate: '2026-11-09',
  revision,
  packageVersion: '0.1.0',
  sourcePolicy:
    'The accepted reference is this commit, not future HEAD. All listed working-tree source bytes matched Git before rebuilding fixtures.',
  intentionalWorkingTreeChanges: [
    {
      path: 'docs/V2_UPGRADE_PLAN.md',
      sha256: digest(readFileSync(join(root, 'docs/V2_UPGRADE_PLAN.md'))),
      disposition:
        'Confirmed implementation plan retained as an intentional documentation-only overlay; not discarded and not part of the v1 executable baseline.',
    },
  ],
  generationCommand: 'node native/scripts/capture-v1.mjs --output native/contracts/accepted-v1',
  referenceBuildCommand: 'pnpm exec tsc -b --force --pretty false',
  generatorRuntime: {
    node: process.version,
    pnpm: execFileSync('pnpm', ['--version'], { cwd: root, encoding: 'utf8' }).trim(),
    platform: process.platform,
    arch: process.arch,
  },
  sources,
  fixtures: Object.entries(files).map(([path, bytes]) => ({ path, sha256: digest(bytes) })),
  limitations: [
    'These fixtures prove v1 reference capture only; they do not prove native parity.',
    'CLI help is a Linux host reference; OS-specific default-path and service behavior still need target-host tests.',
    'No live services, credentials, cluster data, installer or release publication was used.',
  ],
};
if (checking) {
  if (JSON.stringify(sources) !== JSON.stringify(baseline.sources))
    throw new Error('Baseline source index changed');
  for (const [name, bytes] of Object.entries(files)) {
    const frozen = readFileSync(join(destination, name), 'utf8');
    const expected = baseline.fixtures.find((fixture) => fixture.path === name)?.sha256;
    if (frozen !== bytes || digest(frozen) !== expected)
      throw new Error(`Fixture mismatch: ${name}`);
  }
  process.stdout.write(
    `Verified ${sources.length} source hashes and ${Object.keys(files).length} frozen fixtures at ${revision}\n`,
  );
} else {
  mkdirSync(destination, { recursive: true });
  for (const [name, bytes] of Object.entries(files)) writeFileSync(join(destination, name), bytes);
  writeFileSync(
    join(destination, 'baseline.json'),
    await prettier.format(JSON.stringify(metadata), formatting),
  );
  process.stdout.write(
    `Captured ${sources.length} source hashes and ${Object.keys(files).length} fixtures in ${destination}\n`,
  );
}

#!/usr/bin/env node
// Independent Commander oracle: native rendering never imports these expected strings.
import { writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
const reference = resolve(process.argv[2] ?? '');
const helpOutput = process.argv[3],
  processOutput = process.argv[4];
if (!helpOutput || !processOutput)
  throw new Error('Usage: capture-cli-acceptance.mjs FROZEN_CHECKOUT HELP_OUTPUT PROCESS_OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216') throw new Error('Wrong reference');
Object.assign(process.env, {
  GRIDSTATOR_CONFIG: '/V1_CONFIG/client.json',
  GRIDSTATOR_CONTROLLER_DATA: '/V1_DATA/controller',
  GRIDSTATOR_AGENT_DATA: '/V1_DATA/agent',
  NO_COLOR: '1',
  FORCE_COLOR: '0',
});
await import(pathToFileURL(join(reference, 'native/scripts/reference-hostname.cjs')));
const { Command } = await import(
  pathToFileURL(join(reference, 'apps/cli/node_modules/commander/index.js'))
);
const program = new Command()
  .name('gridstator')
  .description(
    'Gridstator: multiple machines, one workflow. Run builds and tests on your LAN cluster.',
  )
  .version('0.1.0', '-v, --version');
for (const [file, name] of [
  ['client', 'registerClientCommands'],
  ['agents', 'registerAgentsCommands'],
  ['workloads', 'registerWorkloadCommands'],
  ['doctor', 'registerDoctorCommand'],
  ['server', 'registerServerCommands'],
  ['service', 'registerServiceCommands'],
]) {
  const module = await import(pathToFileURL(join(reference, `apps/cli/dist/commands/${file}.js`)));
  module[name](program);
}
const cases = [];
const paths = [];
function walk(command, path = []) {
  const exceptions = [];
  if (path.join(' ') === 'agents set') {
    command.options = command.options.filter((o) => o.long !== '--node');
    exceptions.push('native-no-node-runtime-management');
  }
  if (path.join(' ') === 'controller publish-build') {
    command.description(
      'Publish a verified native release bundle (manifest and four target archives), drain the controller and restart (admin)',
    );
    command.registeredArguments[0].description =
      'native release bundle (.tar.gz); defaults to GRIDSTATOR_RELEASE_BUNDLE';
    exceptions.push('native-artifact-and-update-identity');
  }
  if (path.at(-1) === 'install-service') {
    command.description(
      'Install an isolated native service: systemd on Linux, launchd on macOS. User services start at login; --system starts at boot.',
    );
    command.options.find((o) => o.long === '--print').description =
      'print the service definition and exit';
    exceptions.push('mac-platform-and-service-metadata', 'isolated-native-development-state');
  }
  if (path.at(-1) === 'uninstall-service') {
    command.description(`Stop and remove the isolated native ${path[0]} service (keeps data)`);
    exceptions.push('mac-platform-and-service-metadata', 'isolated-native-development-state');
  }
  paths.push([command, path, exceptions]);
  for (const child of command.commands) walk(child, [...path, child.name()]);
}
walk(program);
for (const [command, path, exceptions] of paths)
  cases.push({
    path,
    exceptions: [
      ...new Set([
        ...exceptions,
        ...paths
          .filter(
            ([, childPath]) =>
              childPath.length === path.length + 1 &&
              childPath.slice(0, path.length).every((part, index) => part === path[index]),
          )
          .flatMap(([, , scope]) => scope),
      ]),
    ],
    stdout: command.helpInformation().replaceAll('fixture-host', '<HOSTNAME>'),
    stderr: '',
    exitCode: 0,
  });
writeFileSync(
  helpOutput,
  JSON.stringify(
    {
      sourceRevision: revision,
      policy:
        'Expected Commander output with only enumerated plan-approved Node/runtime/native-artifact/service changes. Paths/hostname normalized as accepted-v1.',
      cases,
    },
    null,
    2,
  ) + '\n',
);
const argvCases = [
  [],
  ['--version'],
  ['run', '--version'],
  ['help'],
  ['help', 'run'],
  ['controller'],
  ['controller', 'local'],
  ['nodes', '--help'],
  ['__unknown_command__'],
  ['bild'],
  ['run', '--__unknown_flag__'],
  ['run', '--imge'],
  ['login'],
  ['login', 'https://localhost'],
  ['login', '--token'],
  ['login', 'https://localhost', '--fingerprint', 'pin'],
  ['login', '--fingerprint', 'pin', '--token', 'token'],
  ['cancel'],
  ['cancel', 'one', 'two'],
  ['status', 'unexpected'],
  ['nodes', 'help'],
  ['nodes', 'unknown'],
  ['run', '--image'],
  ['run', '--task'],
  ['run', '--json=false'],
  ['agent', 'join'],
  ['agent', 'join', 'https://localhost', '--token', 'token'],
  ['controller', 'local', 'configure'],
  ['controller', 'local', 'configure', '--host', '127.0.0.1'],
  ['controller', 'token', 'create'],
  ['controller', 'token', 'create', '--name'],
  ['actions', 'run'],
  ['actions', '__unknown__'],
  ['agents', 'set', '--window'],
  ['logs'],
];
for (const [command, path] of paths) {
  argvCases.push([...path, '--__unknown_flag__']);
  for (const option of command.options) {
    if (option.required) argvCases.push([...path, option.long]);
  }
  if (
    command.registeredArguments.some((argument) => argument.required) ||
    command.options.some((option) => option.mandatory) ||
    (!command._actionHandler && command.commands.length)
  )
    argvCases.push(path);
}
for (const prefix of [[], ['run'], ['controller']])
  for (const flag of ['-hv', '-hV', '-hfoo', '-vh', '-vq', '-qv', '-q', '-vv'])
    argvCases.push([...prefix, flag]);
for (const flag of ['-dh', '-dhv', '-dv', '-dqv', '-de']) argvCases.push(['run', flag]);
argvCases.push(
  ['help', 'controller', 'init'],
  ['controller', 'help', 'init'],
  ['run', '--__unknown_flag__', '--help'],
  ['login', '--token', '-hv'],
);
const uniqueArgv = [...new Map(argvCases.map((argv) => [JSON.stringify(argv), argv])).values()];
const temporary = mkdtempSync(join(tmpdir(), 'gridstator-cli-reference-'));
const processCases = [];
try {
  for (const argv of uniqueArgv) {
    const result = spawnSync(
      process.execPath,
      [
        '--require',
        join(reference, 'native/scripts/reference-hostname.cjs'),
        join(reference, 'apps/cli/dist/main.js'),
        ...argv,
      ],
      {
        cwd: temporary,
        env: {
          ...process.env,
          HOME: temporary,
          XDG_CONFIG_HOME: join(temporary, 'config'),
          GRIDSTATOR_CONFIG: join(temporary, 'absent.json'),
          GRIDSTATOR_URL: '',
          GRIDSTATOR_TOKEN: '',
          GRIDSTATOR_CA_FILE: '',
        },
        encoding: 'utf8',
        timeout: 15000,
      },
    );
    if (result.error) throw result.error;
    processCases.push({
      argv,
      exitCode: result.status,
      stdout: result.stdout.replaceAll('fixture-host', '<HOSTNAME>'),
      stderr: result.stderr.replaceAll('fixture-host', '<HOSTNAME>'),
    });
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
writeFileSync(
  processOutput,
  JSON.stringify({ sourceRevision: revision, cases: processCases }, null, 2) + '\n',
);
process.stdout.write(
  `Captured ${cases.length} Commander help and ${processCases.length} process cases\n`,
);

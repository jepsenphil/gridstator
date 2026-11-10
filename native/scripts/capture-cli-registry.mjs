#!/usr/bin/env node
// Production command declarations only. Expected/rendered help remains in accepted-v1.
import { writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
const reference = resolve(process.argv[2] ?? '');
const output = process.argv[3];
if (!output) throw new Error('Usage: capture-cli-registry.mjs FROZEN_CHECKOUT OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216')
  throw new Error('Use the accepted v1 checkout');
Object.assign(process.env, {
  GRIDSTATOR_CONFIG: '/V1_CONFIG/client.json',
  GRIDSTATOR_CONTROLLER_DATA: '/V1_DATA/controller',
  GRIDSTATOR_AGENT_DATA: '/V1_DATA/agent',
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
function walk(command, path = []) {
  cases.push({
    path,
    description: command.description(),
    hasAction: Boolean(command._actionHandler),
    implicitHelp: Boolean(command._getHelpCommand()),
    usage: command.usage(),
    aliases: command.aliases(),
    arguments: command.registeredArguments.map((a) => ({
      name: a.name(),
      description: a.description,
      required: a.required,
      variadic: a.variadic,
    })),
    options: command.options.map((o) => ({
      flags: o.flags,
      description: o.description,
      mandatory: o.mandatory,
      required: o.required,
      optional: o.optional,
      variadic: o.variadic,
      ...(o.defaultValue === undefined
        ? {}
        : {
            default:
              typeof o.defaultValue === 'string'
                ? o.defaultValue.replaceAll('fixture-host', '<HOSTNAME>')
                : o.defaultValue,
          }),
    })),
  });
  for (const child of command.commands) walk(child, [...path, child.name()]);
}
walk(program);
writeFileSync(
  output,
  JSON.stringify(
    {
      sourceRevision: revision,
      policy:
        'Declarative command metadata, not expected help bytes. The native registry and renderer share these declarations; accepted-v1 remains the independent presentation oracle.',
      cases,
    },
    null,
    2,
  ) + '\n',
);

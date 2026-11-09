#!/usr/bin/env node
import { Command, CommanderError } from 'commander';
import { GRIDSTATOR_VERSION, GridstatorError } from '@gridstator/shared';
import { registerClientCommands, exitState } from './commands/client.js';
import { registerDoctorCommand } from './commands/doctor.js';
import { registerServerCommands } from './commands/server.js';
import { registerWorkloadCommands } from './commands/workloads.js';
import { registerServiceCommands } from './commands/service.js';
import { c } from './output.js';

// node:sqlite still prints an ExperimentalWarning on some Node versions; it is expected here.
const emitWarning = process.emitWarning.bind(process);
process.emitWarning = (warning: string | Error, ...args: unknown[]) => {
  const text = typeof warning === 'string' ? warning : warning.message;
  if (/SQLite is an experimental feature/.test(text)) return;
  (emitWarning as (...a: unknown[]) => void)(warning, ...args);
};

const program = new Command();
program
  .name('gridstator')
  .description(
    'Gridstator: multiple machines, one workflow. Run builds and tests on your LAN cluster.',
  )
  .version(GRIDSTATOR_VERSION, '-v, --version')
  .showHelpAfterError('(run with --help for usage)')
  .exitOverride();

registerClientCommands(program);
registerWorkloadCommands(program);
registerDoctorCommand(program);
registerServerCommands(program);
registerServiceCommands(program);

function fail(err: unknown): never {
  if (err instanceof GridstatorError) {
    process.stderr.write(`${c.red('error:', process.stderr)} ${err.message}\n`);
    if (err.hint) process.stderr.write(`${c.dim(`hint: ${err.hint}`, process.stderr)}\n`);
    process.exit(err.exitCode);
  }
  if (err instanceof CommanderError) {
    process.exit(err.exitCode === 0 ? 0 : 2);
  }
  const e = err as Error;
  process.stderr.write(`${c.red('error:', process.stderr)} ${e.message ?? String(err)}\n`);
  if (process.env['GRIDSTATOR_DEBUG']) process.stderr.write(`${e.stack ?? ''}\n`);
  process.exit(1);
}

try {
  await program.parseAsync(process.argv);
  // Long-running commands (controller/agent start) keep the event loop alive themselves.
  if (exitState.code !== 0) process.exitCode = exitState.code;
} catch (err) {
  fail(err);
}

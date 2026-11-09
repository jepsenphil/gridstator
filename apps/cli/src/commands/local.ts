import type { Command } from 'commander';
import { defaultClientConfigPath, defaultControllerDataDir } from '@gridstator/shared';
import {
  configureLocalController,
  ensureLocalLogin,
  localControllerService,
  localControllerStatus,
} from '../local-controller.js';

interface LocalFlags {
  dataDir: string;
  config: string;
}

export function registerLocalControllerCommands(controller: Command): void {
  const local = controller
    .command('local')
    .description('Manage the controller owned by this account (used by the VS Code extension)');
  const flags = (cmd: Command) =>
    cmd
      .option('--data-dir <dir>', 'local controller data directory', defaultControllerDataDir())
      .option('--config <path>', 'shared client configuration', defaultClientConfigPath())
      .option('--json', 'print JSON (local commands always print JSON)');
  const options = (opts: LocalFlags) => ({ dataDir: opts.dataDir, configPath: opts.config });
  const print = (value: unknown) => process.stdout.write(`${JSON.stringify(value)}\n`);

  flags(
    local.command('status').description('Inspect local ownership, endpoint and user-service state'),
  ).action(async (opts: LocalFlags) => {
    print(await localControllerStatus(options(opts)));
  });
  flags(
    local
      .command('ensure-login')
      .description('Reuse or create one shared local admin login without printing credentials'),
  )
    .option('--restore', 'explicitly replace a revoked or expired local login')
    .action(async (opts: LocalFlags & { restore?: boolean }) => {
      print(await ensureLocalLogin(options(opts), opts.restore ?? false));
    });
  flags(local.command('service').description('Control the local controller user service'))
    .argument('<action>', 'start, stop or restart')
    .action(async (action: string, opts: LocalFlags) => {
      if (!['start', 'stop', 'restart'].includes(action))
        throw new Error('Service action must be start, stop or restart');
      print(await localControllerService(options(opts), action as 'start' | 'stop' | 'restart'));
    });
  flags(
    local
      .command('configure')
      .description('Apply a coordinated endpoint change and restart, recovering on failure'),
  )
    .requiredOption('--host <address>', 'address to listen on')
    .requiredOption('--port <port>', 'TCP port')
    .requiredOption('--url <url>', 'HTTPS controller URL reachable by every worker')
    .action(async (opts: LocalFlags & { host: string; port: string; url: string }) => {
      print(
        await configureLocalController(options(opts), {
          host: opts.host,
          port: Number(opts.port),
          url: opts.url,
        }),
      );
    });
}

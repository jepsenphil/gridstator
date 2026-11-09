import { existsSync } from 'node:fs';
import { join } from 'node:path';
import type { Command } from 'commander';
import {
  GridstatorError,
  createLogger,
  defaultAgentDataDir,
  defaultClientConfigPath,
  defaultControllerDataDir,
  parseBytes,
  shortFingerprint,
  type Logger,
} from '@gridstator/shared';
import { initController, startController, type RunningController } from '@gridstator/controller';
import {
  Agent,
  collectNodeInfo,
  defaultNodeName,
  joinCluster,
  readAgentState,
  writeAgentState,
} from '@gridstator/agent';
import { readClientConfig, writeClientConfig } from '@gridstator/client';
import { DockerCli } from '@gridstator/executors';
import { c, info, lanAddress } from '../output.js';
import { addSettingsFlags, settingsFromFlags, type SettingsFlags } from '../settings-flags.js';

function onShutdown(handler: (signal: string) => Promise<void>, logger: Logger): void {
  let shuttingDown = false;
  const run = (signal: string) => {
    if (shuttingDown) {
      logger.warn('forced exit');
      process.exit(1);
    }
    shuttingDown = true;
    logger.info({ signal }, 'shutting down (press Ctrl-C again to force)');
    handler(signal)
      .then(() => process.exit(0))
      .catch((err: unknown) => {
        logger.error({ err }, 'error during shutdown');
        process.exit(1);
      });
  };
  process.on('SIGINT', () => run('SIGINT'));
  process.on('SIGTERM', () => run('SIGTERM'));
}

export function registerServerCommands(program: Command): void {
  const controller = program
    .command('controller')
    .description('Run and manage the cluster controller');

  controller
    .command('init')
    .description('Create a new cluster: CA, server certificate, database and an admin token')
    .option('--data-dir <dir>', 'controller data directory', defaultControllerDataDir())
    .option('--host <host>', 'address to listen on', '0.0.0.0')
    .option('--port <port>', 'port to listen on', '7300')
    .option('--san <names...>', 'extra hostnames/IPs for the server certificate')
    .option(
      '--force',
      'create a new cluster even if one exists (the old data directory is backed up)',
    )
    .option(
      '--no-login',
      "do not save the admin credentials as this machine's client configuration",
    )
    .action(
      async (opts: {
        dataDir: string;
        host: string;
        port: string;
        san?: string[];
        force?: boolean;
        login: boolean;
      }) => {
        const result = await initController({
          dataDir: opts.dataDir,
          host: opts.host,
          port: Number(opts.port),
          force: opts.force ?? false,
          extraNames: opts.san ?? [],
        });
        const configPath = defaultClientConfigPath();
        let saved = false;
        if (
          opts.login &&
          (opts.force || !(await readClientConfig(configPath).catch(() => undefined)))
        ) {
          await writeClientConfig(
            {
              version: 1,
              url: `https://127.0.0.1:${result.port}`,
              caPem: result.caPem,
              caFingerprint: result.fingerprint,
              token: result.adminToken,
              tokenName: 'admin',
            },
            configPath,
          );
          saved = true;
        }
        const host = lanAddress() ?? '<this-host>';
        info(c.green('Gridstator cluster initialized.', process.stderr));
        if (result.backupDir) info(`  Previous cluster moved to ${result.backupDir}`);
        info(`  Data directory:  ${opts.dataDir}`);
        info(`  CA fingerprint:  ${result.fingerprint}`);
        info(`  Admin token:     ${result.adminToken}`);
        info(
          saved
            ? `  Saved as this machine's client configuration (${configPath}).`
            : '  Keep the admin token safe; it is shown only once.',
        );
        info('');
        info(c.bold('Next steps:', process.stderr));
        info(`  1. Start the controller (and use this machine as a worker too):`);
        info(`       gridstator controller start --with-agent`);
        info(`  2. Add a worker: run \`gridstator token create --join\` here, then the printed`);
        info(`     \`gridstator agent join ...\` command on the worker.`);
        info(`  3. Connect another client machine:`);
        info(`       gridstator token create --name my-laptop`);
        info(
          `       gridstator login https://${host}:${result.port} --fingerprint ${result.fingerprint} --token <token>`,
        );
      },
    );

  addSettingsFlags(
    controller
      .command('start')
      .description('Run the controller in the foreground')
      .option('--data-dir <dir>', 'controller data directory', defaultControllerDataDir())
      .option('--host <host>', 'address to listen on (overrides controller.json)')
      .option('--port <port>', 'port to listen on (overrides controller.json)')
      .option('--with-agent', 'also run a worker agent in this process (embedded mode)')
      .option('--agent-name <name>', 'node name for the embedded agent', defaultNodeName()),
  ).action(
    async (
      opts: SettingsFlags & {
        dataDir: string;
        host?: string;
        port?: string;
        withAgent?: boolean;
        agentName: string;
      },
    ) => {
      const logger = createLogger({ name: 'controller' });
      let ctrl: RunningController;
      try {
        ctrl = await startController({
          dataDir: opts.dataDir,
          ...(opts.host ? { host: opts.host } : {}),
          ...(opts.port ? { port: Number(opts.port) } : {}),
          logger,
        });
      } catch (err) {
        if (err instanceof GridstatorError) throw err;
        throw new GridstatorError(
          'INTERNAL',
          `Controller failed to start: ${(err as Error).message}`,
          { cause: err },
        );
      }
      logger.info(`CA fingerprint ${ctrl.fingerprint} (${shortFingerprint(ctrl.fingerprint)})`);
      let agent: Agent | undefined;
      if (opts.withAgent) {
        const agentDir = join(opts.dataDir, 'agent');
        let state = await readAgentState(agentDir);
        if (!state || state.controllerUrl !== `https://127.0.0.1:${ctrl.port}`) {
          const docker = new DockerCli();
          const nodeInfo = await collectNodeInfo(docker);
          const reg = ctrl.registerLocalAgent(opts.agentName, nodeInfo, settingsFromFlags(opts));
          state = {
            version: 1,
            controllerUrl: `https://127.0.0.1:${ctrl.port}`,
            caPem: ctrl.caPem,
            caFingerprint: ctrl.fingerprint,
            nodeId: reg.nodeId,
            name: opts.agentName,
            credential: reg.credential,
          };
          await writeAgentState(agentDir, state);
        }
        const settings = settingsFromFlags(opts);
        agent = new Agent({
          dataDir: agentDir,
          state,
          ...(settings ? { settings } : {}),
          logger: createLogger({ name: 'agent' }),
        });
        await agent.start();
      }
      onShutdown(async () => {
        await agent?.stop();
        await ctrl.close();
      }, logger);
    },
  );

  const agent = program.command('agent').description('Run a worker agent on this machine');

  addSettingsFlags(
    agent
      .command('join')
      .description('Register this machine with a controller')
      .argument('<url>', 'controller URL, e.g. https://192.168.1.20:7300')
      .requiredOption('--token <token>', 'join token from `gridstator token create --join`')
      .requiredOption('--fingerprint <sha256>', 'cluster CA fingerprint')
      .option('--name <name>', 'node name', defaultNodeName())
      .option('--data-dir <dir>', 'agent data directory', defaultAgentDataDir())
      .option('--force', 'join again even if this machine already joined a cluster'),
  ).action(
    async (
      url: string,
      opts: SettingsFlags & {
        token: string;
        fingerprint: string;
        name: string;
        dataDir: string;
        force?: boolean;
      },
    ) => {
      const settings = settingsFromFlags(opts);
      const state = await joinCluster({
        url,
        joinToken: opts.token,
        fingerprint: opts.fingerprint,
        dataDir: opts.dataDir,
        name: opts.name,
        ...(settings ? { settings } : {}),
        force: opts.force ?? false,
      });
      info(
        c.green(`Joined the cluster as node "${state.name}" (${state.nodeId}).`, process.stderr),
      );
      info(
        `Start the agent with:  gridstator agent start${opts.dataDir !== defaultAgentDataDir() ? ` --data-dir ${opts.dataDir}` : ''}`,
      );
    },
  );

  addSettingsFlags(
    agent
      .command('start')
      .description('Run the agent in the foreground')
      .option('--data-dir <dir>', 'agent data directory', defaultAgentDataDir())
      .option('--cache-size <size>', 'maximum snapshot cache size', '10g')
      .option('--keep-failed', 'keep containers and volumes of failed jobs for inspection')
      .option('--drain', 'on shutdown, wait for running jobs instead of stopping them'),
  ).action(
    async (
      opts: SettingsFlags & {
        dataDir: string;
        cacheSize: string;
        keepFailed?: boolean;
        drain?: boolean;
      },
    ) => {
      const logger = createLogger({ name: 'agent' });
      const state = await readAgentState(opts.dataDir);
      if (!state) {
        const hint = existsSync(opts.dataDir) ? '' : ` (${opts.dataDir} does not exist)`;
        throw new GridstatorError('CONFIG', `This machine has not joined a cluster${hint}`, {
          hint: 'Run `gridstator agent join <url> --token <join-token> --fingerprint <sha256:...>` first.',
        });
      }
      const settings = settingsFromFlags(opts);
      const a = new Agent({
        dataDir: opts.dataDir,
        state,
        ...(settings ? { settings } : {}),
        logger,
        cacheMaxBytes: parseBytes(opts.cacheSize),
        keepFailedWorkspaces: opts.keepFailed ?? false,
      });
      a.onStatus((status, detail) => {
        if (status === 'stopped' && detail) {
          logger.error(detail);
          void a.stop().then(() => process.exit(4));
        }
      });
      await a.start();
      onShutdown(async () => a.stop({ drain: opts.drain ?? false }), logger);
    },
  );
}

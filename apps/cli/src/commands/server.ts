import { createHash } from 'node:crypto';
import { createReadStream, existsSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import type { Command } from 'commander';
import {
  GRIDSTATOR_VERSION,
  GridstatorError,
  createLogger,
  detectBuild,
  packPackage,
  defaultAgentDataDir,
  defaultClientConfigPath,
  defaultControllerDataDir,
  parseBytes,
  shortFingerprint,
  type Logger,
} from '@gridstator/shared';
import {
  initController,
  issueClientTokenOffline,
  startController,
  type RunningController,
} from '@gridstator/controller';
import {
  Agent,
  collectNodeInfo,
  defaultNodeName,
  joinCluster,
  RESTART_EXIT_CODE,
  readAgentState,
  writeAgentState,
} from '@gridstator/agent';
import { readClientConfig, writeClientConfig } from '@gridstator/client';
import { DockerCli } from '@gridstator/executors';
import { c, info, lanAddress } from '../output.js';
import { withClient } from './client.js';
import { addSettingsFlags, settingsFromFlags, type SettingsFlags } from '../settings-flags.js';
import { registerLocalControllerCommands } from './local.js';

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
  registerLocalControllerCommands(controller);

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
      let agent: Agent | undefined;
      // After switching to a published build: stop cleanly and let systemd start the new one.
      const restart = () => {
        void (async () => {
          await agent?.stop({ drain: true }).catch(() => undefined);
          await ctrl?.close().catch(() => undefined);
          process.exit(RESTART_EXIT_CODE);
        })();
      };
      try {
        ctrl = await startController({
          dataDir: opts.dataDir,
          ...(opts.host ? { host: opts.host } : {}),
          ...(opts.port ? { port: Number(opts.port) } : {}),
          logger,
          onRestart: restart,
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

  controller
    .command('publish-build')
    .description(
      "Switch the controller to another build, which workers then follow: this CLI's own bundled build, or a gridstator .tgz (admin)",
    )
    .argument('[tarball]', 'gridstator package (.tgz); default: the build running this command')
    .option('--no-wait', 'do not wait for the controller to come back on the new build')
    .action(async (tarball: string | undefined, opts: { wait: boolean }) =>
      withClient(async (client) => {
        let file = tarball;
        let tmp: string | undefined;
        if (!file) {
          const build = await detectBuild(GRIDSTATOR_VERSION);
          if (!build.packageDir)
            throw new GridstatorError('CONFIG', 'This gridstator is an unbundled workspace build', {
              hint: 'Run `pnpm bundle` and use apps/cli/release/dist/gridstator.mjs, or pass a .tgz.',
            });
          tmp = await mkdtemp(join(tmpdir(), 'gridstator-publish-'));
          file = join(tmp, `gridstator-${build.id}.tgz`);
          await packPackage(build.packageDir, file);
        }
        try {
          const hash = createHash('sha256');
          await pipeline(createReadStream(file), hash);
          const res = await client.publishBuild(createReadStream(file), hash.digest('hex'));
          if (res.action === 'none') {
            info(c.green(`The controller already runs ${res.to}.`, process.stderr));
            return;
          }
          info(`Controller is switching from ${res.from} to ${res.to} and restarting.`);
          if (!opts.wait) return;
          const deadline = Date.now() + 3 * 60_000;
          let wentDown = false;
          for (;;) {
            await new Promise((r) => setTimeout(r, 2000));
            const view = await client.cluster().catch(() => undefined);
            if (!view) wentDown = true;
            else if (view.build?.id === res.to) break;
            else if (wentDown && view.build?.id === res.from)
              throw new GridstatorError(
                'INTERNAL',
                `The controller came back on ${res.from}: the launcher rolled the update back`,
                { hint: 'See the controller service log (journalctl).' },
              );
            if (Date.now() > deadline)
              throw new GridstatorError(
                'UNAVAILABLE',
                `The controller did not come back on ${res.to}`,
                { hint: 'See the controller service log (journalctl).' },
              );
          }
          info(
            c.green(
              `The controller runs ${res.to}. Workers follow it as auto-update allows (see \`gridstator agents status\`).`,
              process.stderr,
            ),
          );
        } finally {
          if (tmp) await rm(tmp, { recursive: true, force: true });
        }
      }),
    );

  controller
    .command('token')
    .description('Recovery: issue client tokens directly in the controller database')
    .command('create')
    .description(
      'Issue a client token without an admin token (needs access to the data directory; works while the controller runs)',
    )
    .requiredOption('--name <name>', 'token name, e.g. "admin-recovered"')
    .option('--role <role>', 'admin or operator', 'admin')
    .option('--data-dir <dir>', 'controller data directory', defaultControllerDataDir())
    .option('--login', "save it as this machine's client login (replaces the current one)")
    .action(async (opts: { name: string; role: string; dataDir: string; login?: boolean }) => {
      if (opts.role !== 'admin' && opts.role !== 'operator')
        throw new GridstatorError('BAD_REQUEST', '--role must be admin or operator');
      const issued = await issueClientTokenOffline({
        dataDir: opts.dataDir,
        name: opts.name,
        role: opts.role,
      });
      if (opts.login) {
        await writeClientConfig(
          {
            version: 1,
            url: issued.url,
            caPem: issued.caPem,
            caFingerprint: issued.fingerprint,
            token: issued.token,
            tokenName: opts.name,
          },
          defaultClientConfigPath(),
        );
        info(
          c.green(`Logged in to ${issued.url} as "${opts.name}" (${opts.role}).`, process.stderr),
        );
      } else {
        process.stdout.write(`${issued.token}\n`);
        info(
          `Log in with: gridstator login ${issued.url} --fingerprint ${issued.fingerprint} --token <the token above>`,
        );
      }
    });

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
        // Updates replace the version the launcher starts, so only managed installs take them.
        selfUpdate: true,
      });
      a.onStatus((status, detail) => {
        if (status === 'stopped' && detail) {
          logger.error(detail);
          void a.stop().then(() => process.exit(4));
        } else if (status === 'restarting') {
          // systemd (Restart=on-failure) starts the launcher again, which runs the new version.
          void a.stop().then(() => process.exit(RESTART_EXIT_CODE));
        }
      });
      await a.start();
      onShutdown(async () => a.stop({ drain: opts.drain ?? false }), logger);
    },
  );
}

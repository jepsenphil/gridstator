import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { rm } from 'node:fs/promises';
import { join } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { GridstatorClient } from '@gridstator/client';
import {
  activateVersion,
  confirmPending,
  errorMessage,
  installFromTarball,
  readInstallState,
  refreshLauncher,
  type BuildIdentity,
  type Logger,
} from '@gridstator/shared';
import type { AgentBuild, AgentTools } from '@gridstator/protocol';

/** Exit code that asks the service manager to start the agent again (now on the new version). */
export const RESTART_EXIT_CODE = 75;

/** How often an agent that cannot join (protocol mismatch) asks the controller for an update. */
const UPDATE_ONLY_POLL_MS = 60_000;

export interface UpdaterOptions {
  client: GridstatorClient;
  build: BuildIdentity;
  /** Only agents started from a managed install by `agent start` may replace themselves. */
  selfUpdate: boolean;
  tmpDir: string;
  logger: Logger;
  /** Stop and exit for a restart (update-only mode and the confirmation watchdog). */
  restart: () => void;
}

/**
 * Updates the agent to the controller's build: downloads the package over the update endpoint,
 * checks its digest, installs it into the version store and activates it provisionally. The new
 * version confirms itself once it has joined the controller; until then the launcher rolls it back
 * if it keeps crashing or misses the deadline.
 */
export class Updater {
  private readonly opts: UpdaterOptions;
  private readonly log: Logger;
  private busy = false;
  private restarting = false;
  private pollTimer: NodeJS.Timeout | undefined;
  private watchdog: NodeJS.Timeout | undefined;

  constructor(opts: UpdaterOptions) {
    this.opts = opts;
    this.log = opts.logger;
  }

  get managed(): boolean {
    return this.opts.selfUpdate && this.opts.build.install !== null;
  }

  get buildId(): string {
    return this.opts.build.id;
  }

  /** What NodeInfo reports about this build. */
  async describe(tools?: AgentTools): Promise<AgentBuild> {
    const { build } = this.opts;
    let lastRollback: AgentBuild['lastRollback'] = null;
    if (this.managed && build.install) {
      const state = await readInstallState(build.install.root).catch(() => undefined);
      lastRollback = state?.lastRollback ?? null;
    }
    return {
      id: build.id,
      version: build.version,
      sha256: build.sha256,
      managed: this.managed,
      node: process.version,
      lastRollback,
      ...(tools ? { tools } : {}),
    };
  }

  /**
   * If this version was just installed and still has to prove itself, exits (so the launcher
   * rolls back) when it has not joined the controller by the deadline.
   */
  async startWatchdog(): Promise<void> {
    const install = this.opts.build.install;
    if (!this.managed || !install) return;
    if (await refreshLauncher(install.root).catch(() => false))
      this.log.info('updated the service launcher');
    const state = await readInstallState(install.root).catch(() => undefined);
    const pending = state?.pending;
    if (!pending || pending.id !== install.id || state?.current !== install.id) return;
    this.log.info(
      { version: install.id, deadline: new Date(pending.deadline).toISOString() },
      'running a new version; it is confirmed once the controller accepts this agent',
    );
    const ms = Math.max(0, pending.deadline - Date.now());
    this.watchdog = setTimeout(() => {
      this.log.error(
        { version: install.id },
        'the new version could not join the controller in time; exiting so the launcher rolls back',
      );
      this.opts.restart();
    }, ms);
    this.watchdog.unref();
  }

  /** Called once the controller welcomed this agent. */
  async joined(): Promise<void> {
    this.stopPolling();
    const install = this.opts.build.install;
    if (!this.managed || !install) return;
    if (this.watchdog) clearTimeout(this.watchdog);
    this.watchdog = undefined;
    if (await confirmPending(install.root, install.id).catch(() => false))
      this.log.info({ version: install.id }, 'update confirmed');
  }

  /**
   * The controller refused the agent channel because the protocols differ: poll the update
   * endpoint, whose shape never changes, until the controller hands out an update.
   */
  startUpdateOnly(): boolean {
    if (!this.managed) return false;
    if (this.pollTimer) return true;
    this.log.warn('this agent cannot join the controller; waiting for an update from it');
    const poll = () =>
      void this.check('update-only').then((outcome) => {
        if (outcome === 'restart') this.opts.restart();
      });
    this.pollTimer = setInterval(poll, UPDATE_ONLY_POLL_MS);
    poll();
    return true;
  }

  stopPolling(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = undefined;
  }

  stop(): void {
    this.stopPolling();
    if (this.watchdog) clearTimeout(this.watchdog);
  }

  /**
   * Asks the controller whether to update, and installs the update. `restart` means the new build
   * is installed and activated: the caller stops the agent and exits for a restart. Concurrent
   * calls return `busy`.
   */
  async check(
    mode: 'normal' | 'update-only' = 'normal',
  ): Promise<'none' | 'restart' | 'failed' | 'busy'> {
    if (this.busy || this.restarting) return 'busy';
    this.busy = true;
    const { client, build } = this.opts;
    let target: string | undefined;
    const file = join(this.opts.tmpDir, `update-${process.pid}-${Date.now()}.tgz`);
    try {
      const res = await client.checkAgentUpdate(build.id, mode);
      if (res.action === 'none') {
        this.log.debug({ reason: res.reason }, 'no update');
        return 'none';
      }
      target = res.build.id;
      const install = build.install;
      if (!this.managed || !install)
        throw new Error(
          'this agent is not a managed install; run `gridstator agent install-service --force` once',
        );
      this.log.info({ from: build.id, to: target }, 'downloading update');
      await this.download(target, file, res.package);
      const id = await installFromTarball(install.root, file);
      if (id !== target) throw new Error(`the package contains build ${id}, expected ${target}`);
      await activateVersion(install.root, id, {
        deadline: Date.now() + res.confirmWithinSeconds * 1000,
      });
      this.log.info({ from: build.id, to: id }, 'update installed; restarting');
      this.restarting = true;
      this.stopPolling();
      return 'restart';
    } catch (err) {
      const message = errorMessage(err);
      if (target) {
        this.log.error({ target, err: message }, 'update failed');
        await client
          .reportAgentUpdate(target, message)
          .catch((e: unknown) =>
            this.log.warn({ err: errorMessage(e) }, 'could not report the failed update'),
          );
      } else {
        this.log.warn({ err: message }, 'update check failed');
      }
      return 'failed';
    } finally {
      this.busy = false;
      await rm(file, { force: true });
    }
  }

  private async download(
    buildId: string,
    file: string,
    expected: { sha256: string; size: number },
  ): Promise<void> {
    const res = await this.opts.client.downloadAgentUpdate(buildId);
    const hash = createHash('sha256');
    let size = 0;
    const meter = new Transform({
      transform(chunk: Buffer, _enc, cb) {
        size += chunk.length;
        if (size > expected.size) {
          cb(new Error(`the package is larger than the announced ${expected.size} bytes`));
          return;
        }
        hash.update(chunk);
        cb(null, chunk);
      },
    });
    await pipeline(res, meter, createWriteStream(file, { mode: 0o600 }));
    const digest = hash.digest('hex');
    if (size !== expected.size)
      throw new Error(`downloaded ${size} bytes, expected ${expected.size}`);
    if (digest !== expected.sha256)
      throw new Error(`package digest ${digest} does not match ${expected.sha256}`);
  }
}

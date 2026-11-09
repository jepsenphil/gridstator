import { createHash, randomBytes } from 'node:crypto';
import { createWriteStream, existsSync } from 'node:fs';
import { chmod, copyFile, mkdir, readdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { Transform, type Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import {
  activateVersion,
  confirmPending,
  GridstatorError,
  installFromTarball,
  readInstallState,
  refreshLauncher,
  type BuildIdentity,
  type Logger,
} from '@gridstator/shared';
import type { PublishBuildResponse } from '@gridstator/protocol';
import type { Store } from './db.js';

const MAX_PACKAGE_BYTES = 512 * 1024 * 1024;

/** Written before the controller switches versions; read by the next start. */
interface SwitchRecord {
  from: string;
  to: string;
  at: number;
  /** Consistent copy of the database taken before the switch. */
  snapshot: string;
}

function updatesDir(dataDir: string): string {
  return join(dataDir, 'updates');
}

function switchFile(dataDir: string): string {
  return join(updatesDir(dataDir), 'switch.json');
}

async function readSwitch(dataDir: string): Promise<SwitchRecord | undefined> {
  try {
    return JSON.parse(await readFile(switchFile(dataDir), 'utf8')) as SwitchRecord;
  } catch {
    return undefined;
  }
}

/**
 * Runs before the database is opened. If the launcher rolled back a controller update, the newer
 * version may already have migrated the database, which this version cannot read: put back the
 * copy taken before the switch (the newer database is kept as controller.db.failed-<version>).
 */
export async function recoverAfterRollback(
  dataDir: string,
  dbPath: string,
  build: BuildIdentity,
  log: Logger,
): Promise<void> {
  const rec = await readSwitch(dataDir);
  if (!rec) return;
  if (build.id === rec.to) return; // the new version; confirm() finishes the switch
  const state = build.install ? await readInstallState(build.install.root) : undefined;
  const rb = state?.lastRollback;
  if (
    build.id === rec.from &&
    rb &&
    rb.from === rec.to &&
    rb.at >= rec.at &&
    existsSync(rec.snapshot)
  ) {
    const failed = `${dbPath}.failed-${rec.to}`;
    await rename(dbPath, failed);
    for (const side of ['-wal', '-shm']) {
      if (existsSync(dbPath + side)) await rename(dbPath + side, failed + side);
    }
    await copyFile(rec.snapshot, dbPath);
    await chmod(dbPath, 0o600);
    log.warn(
      { from: rec.to, to: build.id, kept: failed },
      'the controller update was rolled back; restored the database from before the update',
    );
  }
  await rm(switchFile(dataDir), { force: true });
}

/**
 * Controller self-update: `POST /v1/controller/build` hands the controller a new build, which it
 * installs into its own version store and switches to provisionally after copying its database.
 * The service manager restarts it on the new version, which confirms itself once it is serving.
 */
export class SelfUpdater {
  private readonly dataDir: string;
  private readonly build: BuildIdentity;
  private readonly store: Store;
  private readonly log: Logger;
  private readonly onRestart: (() => void) | undefined;
  private readonly confirmWithinSeconds: number;
  private busy = false;
  /** The build a publish switched to; set until this process exits. */
  private switching: string | undefined;

  constructor(deps: {
    dataDir: string;
    build: BuildIdentity;
    store: Store;
    logger: Logger;
    onRestart?: () => void;
    confirmWithinSeconds?: number;
  }) {
    this.dataDir = deps.dataDir;
    this.build = deps.build;
    this.store = deps.store;
    this.log = deps.logger;
    this.onRestart = deps.onRestart;
    this.confirmWithinSeconds = deps.confirmWithinSeconds ?? 600;
  }

  /** Called once the controller is serving: the update (if any) worked. */
  async confirm(): Promise<void> {
    const install = this.build.install;
    if (!install) return;
    if (await refreshLauncher(install.root)) this.log.info('updated the service launcher');
    if (await confirmPending(install.root, install.id))
      this.log.info({ version: install.id }, 'controller update confirmed');
    const rec = await readSwitch(this.dataDir);
    if (rec?.to === install.id) await rm(switchFile(this.dataDir), { force: true });
  }

  async publish(body: Readable, sha256: string): Promise<PublishBuildResponse> {
    const install = this.build.install;
    if (!install) {
      body.resume();
      throw new GridstatorError('CONFLICT', 'The controller is not a managed install', {
        hint: 'Run `gridstator controller install-service --force` on it once (from a bundled build), then publish again.',
      });
    }
    if (!this.onRestart) {
      body.resume();
      throw new GridstatorError(
        'CONFLICT',
        'This controller process cannot restart itself (it is not running as a service)',
      );
    }
    if (this.busy || this.switching) {
      body.resume();
      throw new GridstatorError(
        'CONFLICT',
        this.switching
          ? `The controller is already switching to ${this.switching}`
          : 'Another build is being published',
      );
    }
    this.busy = true;
    const dir = updatesDir(this.dataDir);
    await mkdir(dir, { recursive: true, mode: 0o700 });
    const file = join(dir, `incoming-${randomBytes(6).toString('hex')}.tgz`);
    try {
      const hash = createHash('sha256');
      let size = 0;
      await pipeline(
        body,
        new Transform({
          transform(chunk: Buffer, _enc, cb) {
            size += chunk.length;
            if (size > MAX_PACKAGE_BYTES) return cb(new Error('the package is too large'));
            hash.update(chunk);
            cb(null, chunk);
          },
        }),
        createWriteStream(file, { mode: 0o600 }),
      );
      const digest = hash.digest('hex');
      if (digest !== sha256)
        throw new GridstatorError('BAD_REQUEST', `Upload has sha256 ${digest}, expected ${sha256}`);
      let id: string;
      try {
        id = await installFromTarball(install.root, file);
      } catch (err) {
        throw new GridstatorError(
          'BAD_REQUEST',
          `Not a gridstator package: ${(err as Error).message}`,
        );
      }
      if (id === this.build.id) return { action: 'none', from: this.build.id, to: id };

      // A consistent copy of the database (WAL included) to restore if the new version fails.
      for (const f of await readdir(dir))
        if (f.startsWith('db-before-')) await rm(join(dir, f), { force: true });
      const snapshot = join(dir, `db-before-${id}.sqlite`);
      this.store.db.prepare('VACUUM INTO ?').run(snapshot);
      await chmod(snapshot, 0o600);
      const rec: SwitchRecord = { from: this.build.id, to: id, at: Date.now(), snapshot };
      await writeFile(switchFile(this.dataDir), JSON.stringify(rec), { mode: 0o600 });
      await activateVersion(install.root, id, {
        deadline: Date.now() + this.confirmWithinSeconds * 1000,
      });
      this.log.info({ from: this.build.id, to: id }, 'switching to a published build; restarting');
      this.switching = id;
      const restart = this.onRestart;
      setTimeout(restart, 300).unref();
      return { action: 'restarting', from: this.build.id, to: id };
    } finally {
      this.busy = false;
      await rm(file, { force: true });
    }
  }
}

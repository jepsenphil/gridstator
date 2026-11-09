import { createHash } from 'node:crypto';
import { createReadStream, existsSync } from 'node:fs';
import { mkdir, readdir, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { packPackage, type BuildIdentity, type Logger } from '@gridstator/shared';
import type { BuildRef, ControllerBuild } from '@gridstator/protocol';

export interface UpdatePackage {
  path: string;
  sha256: string;
  size: number;
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash('sha256');
  await pipeline(createReadStream(path), hash);
  return hash.digest('hex');
}

/**
 * The controller's own build, which workers update to. Workers download it as an npm-style
 * tarball packed from the package directory the controller runs from (its version store when it
 * is a managed install). The unbundled workspace build cannot be shipped, so workers cannot
 * update while the controller runs one.
 */
export class BuildSource {
  readonly ref: BuildRef;
  private readonly identity: BuildIdentity;
  private readonly dir: string;
  private readonly log: Logger;
  private pkg: UpdatePackage | undefined;
  private failure: string | undefined;
  private preparing: Promise<UpdatePackage | undefined> | undefined;

  constructor(identity: BuildIdentity, dataDir: string, logger: Logger) {
    this.identity = identity;
    this.ref = { id: identity.id, version: identity.version, sha256: identity.sha256 };
    // Only packages live here: it is cleaned of other builds (self-update uses updates/ itself).
    this.dir = join(dataDir, 'updates', 'packages');
    this.log = logger;
  }

  view(): ControllerBuild {
    const reason = !this.identity.packageDir
      ? 'the controller runs an unbundled workspace build; install a bundled build to update workers'
      : (this.failure ?? (this.pkg ? null : 'the update package is still being prepared'));
    return { ...this.ref, updatable: this.pkg !== undefined, reason };
  }

  /** The package for this build, if it is ready. */
  get package(): UpdatePackage | undefined {
    return this.pkg;
  }

  /** Packs the running build (once) and removes packages of other builds. */
  prepare(): Promise<UpdatePackage | undefined> {
    this.preparing ??= this.doPrepare();
    return this.preparing;
  }

  private async doPrepare(): Promise<UpdatePackage | undefined> {
    const pkgDir = this.identity.packageDir;
    if (!pkgDir) return undefined;
    try {
      await mkdir(this.dir, { recursive: true, mode: 0o700 });
      const path = join(this.dir, `${this.ref.id}.tgz`);
      if (!existsSync(path)) await packPackage(pkgDir, path);
      for (const name of await readdir(this.dir)) {
        if (name !== `${this.ref.id}.tgz`)
          await rm(join(this.dir, name), { recursive: true, force: true });
      }
      this.pkg = { path, sha256: await sha256File(path), size: (await stat(path)).size };
      this.log.info({ build: this.ref.id, bytes: this.pkg.size }, 'update package ready');
      return this.pkg;
    } catch (err) {
      this.failure = `could not pack the controller's build: ${(err as Error).message}`;
      this.log.error({ err }, 'could not prepare the update package');
      return undefined;
    }
  }
}

import { execFile } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, realpathSync } from 'node:fs';
import { cp, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { promisify } from 'node:util';

/**
 * Managed installs: each role (controller, agent) keeps its own copies of the gridstator package in
 * `<data dir>/install/versions/<id>/`, owned by the service user, so updates need no root and the
 * previous version stays around for rollback. systemd runs a small launcher that is not replaced
 * by updates; it reads `state.json`, rolls a pending update back when it keeps crashing or is not
 * confirmed in time, and imports the active version's bundled entry point.
 *
 *   <data dir>/install/
 *     launcher.mjs
 *     state.json            { current, previous?, pending?, lastRollback? }
 *     versions/<id>/        an unpacked `gridstator` npm package (package.json, dist/gridstator.mjs)
 *
 * `<id>` is `<package version>-<first 12 hex chars of the entry's sha256>`, so dev builds that share
 * a version number still get distinct directories.
 */

const execFileAsync = promisify(execFile);

/** Bundled entry point inside a gridstator package. */
export const PACKAGE_ENTRY = join('dist', 'gridstator.mjs');
/** Launches of a pending version before the launcher rolls it back. */
export const LAUNCHER_MAX_ATTEMPTS = 3;
/** Bumped whenever the launcher source changes, so newer versions can replace it. */
export const LAUNCHER_VERSION = 2;

export interface PendingSwitch {
  /** The version being tried. */
  id: string;
  /** The version to return to on failure. */
  previous: string;
  /** Launches of `id` so far. */
  attempts: number;
  /** Epoch ms by which the new version must confirm itself. */
  deadline: number;
  /**
   * Set when the switch also changes the Node.js binary: the one to return to (null: the one the
   * service was installed with). A Node-only switch has `id === previous`.
   */
  previousNode?: string | null;
}

export interface RollbackRecord {
  from: string;
  to: string;
  at: number;
  reason: 'crashed' | 'timed-out';
}

export interface InstallState {
  version: 1;
  current: string;
  previous?: string;
  pending?: PendingSwitch;
  lastRollback?: RollbackRecord;
  /** Node.js binary the launcher runs versions with (managed Node); default: its own. */
  node?: string;
}

export function installRoot(dataDir: string): string {
  return join(dataDir, 'install');
}

export function launcherPath(root: string): string {
  return join(root, 'launcher.mjs');
}

function statePath(root: string): string {
  return join(root, 'state.json');
}

export function versionDir(root: string, id: string): string {
  return join(root, 'versions', id);
}

export function versionEntry(root: string, id: string): string {
  return join(versionDir(root, id), PACKAGE_ENTRY);
}

/**
 * The package directory of a bundled gridstator entry script (`<pkg>/dist/gridstator.mjs`), or
 * undefined for anything else, such as the unbundled workspace build that needs `node_modules`.
 */
export function bundledPackageDir(script: string): string | undefined {
  if (basename(script) !== 'gridstator.mjs' || basename(dirname(script)) !== 'dist')
    return undefined;
  const dir = dirname(dirname(script));
  return existsSync(join(dir, 'package.json')) ? dir : undefined;
}

/** What identifies a gridstator build: the package version plus the sha256 of its entry point. */
export interface PackageIdentity {
  /** `<version>-<first 12 hex chars of sha256>`, the version store directory name. */
  id: string;
  version: string;
  sha256: string;
}

/** Reads the identity of an unpacked gridstator package. */
export async function packageIdentity(pkgDir: string): Promise<PackageIdentity> {
  const pkg = JSON.parse(await readFile(join(pkgDir, 'package.json'), 'utf8')) as {
    name?: unknown;
    version?: unknown;
  };
  if (pkg.name !== 'gridstator' || typeof pkg.version !== 'string')
    throw new Error(`${pkgDir} is not a gridstator package`);
  const entry = join(pkgDir, PACKAGE_ENTRY);
  if (!existsSync(entry)) throw new Error(`${pkgDir} has no ${PACKAGE_ENTRY}`);
  const sha256 = createHash('sha256')
    .update(await readFile(entry))
    .digest('hex');
  return { id: `${pkg.version}-${sha256.slice(0, 12)}`, version: pkg.version, sha256 };
}

/** The build the current process runs. */
export interface BuildIdentity {
  /** `<version>-<sha12>`, or `<version>-unbundled` for the workspace build. */
  id: string;
  version: string;
  /** sha256 of the bundled entry point; null for the unbundled workspace build. */
  sha256: string | null;
  /** The unpacked gridstator package this process runs from; null when unbundled. */
  packageDir: string | null;
  /** Set when the process was started by the launcher of a managed install. */
  install: { root: string; id: string } | null;
}

/**
 * Identifies the running build. The launcher of a managed install passes its store and version in
 * GRIDSTATOR_INSTALL_ROOT / GRIDSTATOR_INSTALL_VERSION; otherwise the entry script tells whether
 * this is a bundled package (e.g. a global npm install) or the unbundled workspace build, which
 * cannot be copied to other machines.
 */
export async function detectBuild(
  fallbackVersion: string,
  script: string | undefined = process.argv[1],
  env: NodeJS.ProcessEnv = process.env,
): Promise<BuildIdentity> {
  const root = env['GRIDSTATOR_INSTALL_ROOT'];
  const installed = env['GRIDSTATOR_INSTALL_VERSION'];
  if (root && installed && existsSync(versionEntry(root, installed))) {
    const packageDir = versionDir(root, installed);
    const ident = await packageIdentity(packageDir);
    return { ...ident, packageDir, install: { root, id: installed } };
  }
  let packageDir: string | undefined;
  if (script) {
    try {
      packageDir = bundledPackageDir(realpathSync(script));
    } catch {
      packageDir = undefined;
    }
  }
  if (packageDir) {
    try {
      return { ...(await packageIdentity(packageDir)), packageDir, install: null };
    } catch {
      // not a gridstator package after all
    }
  }
  return {
    id: `${fallbackVersion}-unbundled`,
    version: fallbackVersion,
    sha256: null,
    packageDir: null,
    install: null,
  };
}

/**
 * Packs an unpacked gridstator package as an npm-style tarball (everything under `package/`), the
 * format {@link installFromTarball} accepts.
 */
export async function packPackage(pkgDir: string, tarball: string): Promise<void> {
  const staged = join(dirname(tarball), `.pack-${randomBytes(6).toString('hex')}`);
  await mkdir(staged, { recursive: true });
  try {
    await cp(pkgDir, join(staged, 'package'), { recursive: true });
    const tmp = `${tarball}.${randomBytes(4).toString('hex')}.tmp`;
    await execFileAsync('tar', ['-czf', tmp, '-C', staged, 'package']);
    await rename(tmp, tarball);
  } finally {
    await rm(staged, { recursive: true, force: true });
  }
}

/** Computes the version id of an unpacked gridstator package. */
export async function packageVersionId(pkgDir: string): Promise<string> {
  return (await packageIdentity(pkgDir)).id;
}

export async function readInstallState(root: string): Promise<InstallState | undefined> {
  try {
    return JSON.parse(await readFile(statePath(root), 'utf8')) as InstallState;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw err;
  }
}

async function writeInstallState(root: string, state: InstallState): Promise<void> {
  const file = statePath(root);
  const tmp = `${file}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
  await writeFile(tmp, JSON.stringify(state, null, 2) + '\n', { mode: 0o644 });
  await rename(tmp, file);
}

/** Moves an unpacked package into `versions/<id>` (keeping an existing copy). */
async function adopt(root: string, staged: string): Promise<string> {
  const id = await packageVersionId(staged);
  const dest = versionDir(root, id);
  if (existsSync(join(dest, PACKAGE_ENTRY))) {
    await rm(staged, { recursive: true, force: true });
    return id;
  }
  await rm(dest, { recursive: true, force: true });
  await rename(staged, dest);
  return id;
}

function stagingDir(root: string): string {
  return join(root, 'versions', `.staging-${randomBytes(6).toString('hex')}`);
}

/** Copies an unpacked gridstator package into the store and returns its version id. */
export async function installFromPackageDir(root: string, pkgDir: string): Promise<string> {
  const staged = stagingDir(root);
  await mkdir(dirname(staged), { recursive: true });
  try {
    await cp(pkgDir, staged, { recursive: true });
    return await adopt(root, staged);
  } finally {
    await rm(staged, { recursive: true, force: true });
  }
}

/** Unpacks a `gridstator-<version>.tgz` (as made by `npm pack`) into the store. */
export async function installFromTarball(root: string, tarball: string): Promise<string> {
  const staged = stagingDir(root);
  await mkdir(staged, { recursive: true });
  try {
    await execFileAsync('tar', ['-xzf', tarball, '-C', staged, '--no-same-owner']);
    // npm tarballs keep everything under `package/`.
    return await adopt(root, join(staged, 'package'));
  } finally {
    await rm(staged, { recursive: true, force: true });
  }
}

/**
 * Makes `id` the active version. With `pending`, the switch is provisional: the launcher returns
 * to the current version if `id` fails to start {@link LAUNCHER_MAX_ATTEMPTS} times or does not
 * call {@link confirmPending} before the deadline.
 */
export async function activateVersion(
  root: string,
  id: string,
  pending?: { deadline: number },
): Promise<InstallState> {
  if (!existsSync(versionEntry(root, id))) throw new Error(`version ${id} is not installed`);
  const old = await readInstallState(root);
  const state: InstallState = { version: 1, current: id };
  if (old?.lastRollback) state.lastRollback = old.lastRollback;
  if (old?.node) state.node = old.node;
  if (old && old.current !== id) {
    state.previous = old.current;
    if (pending)
      state.pending = { id, previous: old.current, attempts: 0, deadline: pending.deadline };
  } else if (old?.previous) {
    state.previous = old.previous;
  }
  await writeInstallState(root, state);
  return state;
}

/** Called by a newly started version once it is healthy; returns false if nothing was pending. */
export async function confirmPending(root: string, id: string): Promise<boolean> {
  const state = await readInstallState(root);
  if (!state?.pending || state.pending.id !== id || state.current !== id) return false;
  delete state.pending;
  await writeInstallState(root, state);
  return true;
}

/**
 * Switches the Node.js binary the launcher runs versions with (null: back to the launcher's own).
 * Like a version switch it is provisional: the launcher restores the previous binary if the
 * version keeps crashing on the new one or does not confirm itself before `deadline`. Combines
 * with a pending version switch, so one restart applies both.
 */
export async function setManagedNode(
  root: string,
  nodePath: string | null,
  deadline: number,
): Promise<InstallState> {
  const state = await readInstallState(root);
  if (!state) throw new Error(`${root} has no install state`);
  const old = state.node ?? null;
  if (old === nodePath) return state;
  if (state.pending) {
    if (state.pending.previousNode === undefined) state.pending.previousNode = old;
    state.pending.deadline = Math.max(state.pending.deadline, deadline);
  } else {
    state.pending = {
      id: state.current,
      previous: state.current,
      attempts: 0,
      deadline,
      previousNode: old,
    };
  }
  if (nodePath) state.node = nodePath;
  else delete state.node;
  await writeInstallState(root, state);
  return state;
}

/** Rewrites an older launcher (as a newer version starts), so new launcher features apply. */
export async function refreshLauncher(root: string): Promise<boolean> {
  let current: number;
  try {
    const m = /launcher-version: (\d+)/.exec(await readFile(launcherPath(root), 'utf8'));
    current = m ? Number(m[1]) : 0;
  } catch {
    return false;
  }
  if (current >= LAUNCHER_VERSION) return false;
  await writeLauncher(root);
  return true;
}

/** Writes the launcher (see {@link LAUNCHER_SOURCE}). */
export async function writeLauncher(root: string): Promise<string> {
  const file = launcherPath(root);
  await mkdir(root, { recursive: true });
  await writeFile(file, LAUNCHER_SOURCE, { mode: 0o755 });
  return file;
}

/**
 * The launcher systemd runs. It must only use Node built-ins: it outlives the versions it starts
 * (newer versions replace it via {@link refreshLauncher}). It sets GRIDSTATOR_INSTALL_ROOT /
 * GRIDSTATOR_INSTALL_VERSION so the running version knows it is managed, and re-executes itself
 * (same PID, so systemd is unaffected) under the managed Node.js binary when one is set.
 */
export const LAUNCHER_SOURCE = `#!/usr/bin/env node
// Gridstator launcher (generated by \`gridstator <role> install-service\`; do not edit).
// launcher-version: ${LAUNCHER_VERSION}
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const MAX_ATTEMPTS = ${LAUNCHER_MAX_ATTEMPTS};
const root = dirname(fileURLToPath(import.meta.url));
const statePath = join(root, 'state.json');
const entry = (id) => join(root, 'versions', id, 'dist', 'gridstator.mjs');
const say = (msg) => process.stderr.write('gridstator launcher: ' + msg + '\\n');

let state;
try {
  state = JSON.parse(readFileSync(statePath, 'utf8'));
} catch (err) {
  say('cannot read ' + statePath + ': ' + err.message);
  process.exit(1);
}
const save = () => {
  const tmp = statePath + '.' + process.pid + '.tmp';
  writeFileSync(tmp, JSON.stringify(state, null, 2) + '\\n', { mode: 0o644 });
  renameSync(tmp, statePath);
};

// Second pass: already re-executed under the managed Node.js binary; the first pass counted it.
const reexeced = process.env.GRIDSTATOR_LAUNCHER_REEXEC === '1';
delete process.env.GRIDSTATOR_LAUNCHER_REEXEC;

const p = state.pending;
if (!reexeced && p && p.id === state.current) {
  const reason =
    p.attempts >= MAX_ATTEMPTS ? 'crashed' : Date.now() > p.deadline ? 'timed-out' : undefined;
  if (reason && existsSync(entry(p.previous))) {
    const what = p.id === p.previous ? 'the switch to Node.js ' + state.node : 'update to ' + p.id;
    say(
      what + (reason === 'crashed' ? ' failed to start ' + p.attempts + ' times' : ' was not confirmed in time') +
        '; rolled back to ' + p.previous,
    );
    state.lastRollback = { from: p.id, to: p.previous, at: Date.now(), reason };
    if (p.id !== p.previous) {
      state.current = p.previous;
      delete state.previous;
    }
    if (p.previousNode !== undefined) {
      if (p.previousNode) state.node = p.previousNode;
      else delete state.node;
    }
    delete state.pending;
  } else {
    p.attempts += 1;
  }
  save();
}

if (!reexeced && state.node && state.node !== process.execPath) {
  if (existsSync(state.node) && typeof process.execve === 'function') {
    try {
      process.execve(state.node, [state.node, fileURLToPath(import.meta.url), ...process.argv.slice(2)], {
        ...process.env,
        GRIDSTATOR_LAUNCHER_REEXEC: '1',
      });
    } catch (err) {
      say('cannot run ' + state.node + ': ' + err.message + '; using ' + process.execPath);
    }
  } else {
    say('cannot switch to ' + state.node + '; using ' + process.execPath);
  }
}

const main = entry(state.current);
if (!existsSync(main)) {
  say('version ' + state.current + ' is missing (' + main + ')');
  process.exit(1);
}
process.argv[1] = main;
process.env.GRIDSTATOR_INSTALL_ROOT = root;
process.env.GRIDSTATOR_INSTALL_VERSION = state.current;
await import(pathToFileURL(main).href);
`;

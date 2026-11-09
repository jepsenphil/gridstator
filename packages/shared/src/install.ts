import { execFile } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
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
export const LAUNCHER_VERSION = 1;

export interface PendingSwitch {
  /** The version being tried. */
  id: string;
  /** The version to return to on failure. */
  previous: string;
  /** Launches of `id` so far. */
  attempts: number;
  /** Epoch ms by which the new version must confirm itself. */
  deadline: number;
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

/** Computes the version id of an unpacked gridstator package. */
export async function packageVersionId(pkgDir: string): Promise<string> {
  const pkg = JSON.parse(await readFile(join(pkgDir, 'package.json'), 'utf8')) as {
    name?: unknown;
    version?: unknown;
  };
  if (pkg.name !== 'gridstator' || typeof pkg.version !== 'string')
    throw new Error(`${pkgDir} is not a gridstator package`);
  const entry = join(pkgDir, PACKAGE_ENTRY);
  if (!existsSync(entry)) throw new Error(`${pkgDir} has no ${PACKAGE_ENTRY}`);
  const sha = createHash('sha256')
    .update(await readFile(entry))
    .digest('hex');
  return `${pkg.version}-${sha.slice(0, 12)}`;
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

/** Writes the launcher (see {@link LAUNCHER_SOURCE}). */
export async function writeLauncher(root: string): Promise<string> {
  const file = launcherPath(root);
  await mkdir(root, { recursive: true });
  await writeFile(file, LAUNCHER_SOURCE, { mode: 0o755 });
  return file;
}

/**
 * The launcher systemd runs. It must only use Node built-ins: it is written once and outlives the
 * versions it starts. It sets GRIDSTATOR_INSTALL_ROOT / GRIDSTATOR_INSTALL_VERSION so the running
 * version knows it is managed.
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

const p = state.pending;
if (p && p.id === state.current) {
  const reason =
    p.attempts >= MAX_ATTEMPTS ? 'crashed' : Date.now() > p.deadline ? 'timed-out' : undefined;
  if (reason && existsSync(entry(p.previous))) {
    say(
      'update to ' + p.id + (reason === 'crashed' ? ' failed to start ' + p.attempts + ' times' : ' was not confirmed in time') +
        '; rolled back to ' + p.previous,
    );
    state.lastRollback = { from: p.id, to: p.previous, at: Date.now(), reason };
    state.current = p.previous;
    delete state.previous;
    delete state.pending;
  } else {
    p.attempts += 1;
  }
  save();
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

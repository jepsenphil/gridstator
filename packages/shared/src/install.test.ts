import { execFileSync, spawnSync } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  activateVersion,
  bundledPackageDir,
  confirmPending,
  detectBuild,
  installFromPackageDir,
  installFromTarball,
  LAUNCHER_MAX_ATTEMPTS,
  launcherPath,
  packageIdentity,
  packageVersionId,
  packPackage,
  readInstallState,
  refreshLauncher,
  setManagedNode,
  writeLauncher,
} from './install.js';

let tmp: string;
let root: string;

beforeEach(async () => {
  tmp = await mkdtemp(join(tmpdir(), 'gst-install-'));
  root = join(tmp, 'install');
});
afterEach(async () => {
  await rm(tmp, { recursive: true, force: true });
});

/** A fake gridstator package whose entry records how it was started, or exits with `exitCode`. */
async function fakePackage(name: string, opts: { exitCode?: number } = {}): Promise<string> {
  const dir = join(tmp, 'pkgs', name);
  await mkdir(join(dir, 'dist'), { recursive: true });
  await writeFile(
    join(dir, 'package.json'),
    JSON.stringify({ name: 'gridstator', version: '0.2.0' }),
  );
  const body =
    opts.exitCode !== undefined
      ? `process.exit(${opts.exitCode});`
      : `import { writeFileSync } from 'node:fs';
writeFileSync(process.env.OUT, JSON.stringify({
  name: ${JSON.stringify(name)},
  argv: process.argv.slice(1),
  root: process.env.GRIDSTATOR_INSTALL_ROOT,
  version: process.env.GRIDSTATOR_INSTALL_VERSION,
  node: process.env.FAKE_NODE ?? null,
}));`;
  await writeFile(join(dir, 'dist', 'gridstator.mjs'), body);
  return dir;
}

function launch(...args: string[]) {
  const out = join(tmp, 'out.json');
  const r = spawnSync(process.execPath, [launcherPath(root), ...args], {
    env: { ...process.env, OUT: out },
    encoding: 'utf8',
  });
  return { status: r.status, stderr: r.stderr, out };
}

describe('version store', () => {
  it('ids packages by version and content, and recognises bundled entries', async () => {
    const a = await packageVersionId(await fakePackage('a'));
    const b = await packageVersionId(await fakePackage('b'));
    expect(a).toMatch(/^0\.2\.0-[0-9a-f]{12}$/);
    expect(a).not.toBe(b);
    const pkg = await fakePackage('c');
    expect(bundledPackageDir(join(pkg, 'dist', 'gridstator.mjs'))).toBe(pkg);
    expect(bundledPackageDir('/repo/apps/cli/dist/main.js')).toBeUndefined();
  });

  it('installs from a directory or an npm tarball, idempotently', async () => {
    const pkg = await fakePackage('a');
    const id = await installFromPackageDir(root, pkg);
    expect(await installFromPackageDir(root, pkg)).toBe(id);
    const staging = join(tmp, 'tar');
    await mkdir(staging);
    await rm(join(staging, 'package'), { recursive: true, force: true });
    execFileSync('cp', ['-r', await fakePackage('t'), join(staging, 'package')]);
    const tgz = join(tmp, 'gridstator-0.2.0.tgz');
    execFileSync('tar', ['-czf', tgz, '-C', staging, 'package']);
    const tid = await installFromTarball(root, tgz);
    expect(tid).not.toBe(id);
    expect(await readFile(join(root, 'versions', tid, 'dist', 'gridstator.mjs'), 'utf8')).toContain(
      '"t"',
    );
  });
});

describe('build identity', () => {
  it('identifies a managed install from the launcher environment', async () => {
    const id = await installFromPackageDir(root, await fakePackage('a'));
    const build = await detectBuild('9.9.9', '/somewhere/else.js', {
      GRIDSTATOR_INSTALL_ROOT: root,
      GRIDSTATOR_INSTALL_VERSION: id,
    });
    expect(build).toMatchObject({
      id,
      version: '0.2.0',
      packageDir: join(root, 'versions', id),
      install: { root, id },
    });
    expect(build.sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(id.endsWith(build.sha256!.slice(0, 12))).toBe(true);
  });

  it('identifies a bundled package, and falls back for the workspace build', async () => {
    const pkg = await fakePackage('b');
    const bundled = await detectBuild('9.9.9', join(pkg, 'dist', 'gridstator.mjs'), {});
    expect(bundled).toMatchObject({
      id: (await packageIdentity(pkg)).id,
      packageDir: pkg,
      install: null,
    });
    expect(await detectBuild('9.9.9', '/repo/apps/cli/dist/main.js', {})).toEqual({
      id: '9.9.9-unbundled',
      version: '9.9.9',
      sha256: null,
      packageDir: null,
      install: null,
    });
  });

  it('packs a package into a tarball that installs as the same build', async () => {
    const pkg = await fakePackage('p');
    const tgz = join(tmp, 'out', 'p.tgz');
    await mkdir(join(tmp, 'out'));
    await packPackage(pkg, tgz);
    expect(await installFromTarball(root, tgz)).toBe(await packageVersionId(pkg));
  });
});

describe('launcher', () => {
  it('runs the active version with the original arguments', async () => {
    const id = await installFromPackageDir(root, await fakePackage('a'));
    await activateVersion(root, id);
    await writeLauncher(root);
    const r = launch('agent', 'start', '--data-dir', '/d');
    expect(r.status).toBe(0);
    const out = JSON.parse(await readFile(r.out, 'utf8')) as Record<string, unknown>;
    expect(out).toMatchObject({
      name: 'a',
      argv: [
        join(root, 'versions', id, 'dist', 'gridstator.mjs'),
        'agent',
        'start',
        '--data-dir',
        '/d',
      ],
      root,
      version: id,
    });
  });

  it('rolls back a pending version that keeps crashing', async () => {
    const good = await installFromPackageDir(root, await fakePackage('good'));
    const bad = await installFromPackageDir(root, await fakePackage('bad', { exitCode: 7 }));
    await activateVersion(root, good);
    await activateVersion(root, bad, { deadline: Date.now() + 60_000 });
    await writeLauncher(root);
    for (let i = 0; i < LAUNCHER_MAX_ATTEMPTS; i++) expect(launch().status).toBe(7);
    const r = launch();
    expect(r.status).toBe(0);
    expect(r.stderr).toContain(
      `failed to start ${LAUNCHER_MAX_ATTEMPTS} times; rolled back to ${good}`,
    );
    const state = await readInstallState(root);
    expect(state).toMatchObject({
      current: good,
      lastRollback: { from: bad, to: good, reason: 'crashed' },
    });
    expect(state?.pending).toBeUndefined();
    expect(state?.previous).toBeUndefined();
  });

  it('rolls back a pending version that missed its deadline', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    const b = await installFromPackageDir(root, await fakePackage('b'));
    await activateVersion(root, a);
    await activateVersion(root, b, { deadline: Date.now() - 1 });
    await writeLauncher(root);
    const r = launch();
    expect(r.stderr).toContain('was not confirmed in time');
    expect((JSON.parse(await readFile(r.out, 'utf8')) as { name: string }).name).toBe('a');
  });

  it('keeps a confirmed version', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    const b = await installFromPackageDir(root, await fakePackage('b'));
    await activateVersion(root, a);
    await activateVersion(root, b, { deadline: Date.now() + 60_000 });
    await writeLauncher(root);
    expect(launch().status).toBe(0);
    expect(await confirmPending(root, a)).toBe(false);
    expect(await confirmPending(root, b)).toBe(true);
    for (let i = 0; i < LAUNCHER_MAX_ATTEMPTS + 1; i++) launch();
    expect(await readInstallState(root)).toEqual({ version: 1, current: b, previous: a });
  });

  it('fails clearly when the active version is missing', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    await activateVersion(root, a);
    await writeLauncher(root);
    await rm(join(root, 'versions', a), { recursive: true });
    const r = launch();
    expect(r.status).toBe(1);
    expect(r.stderr).toContain(`version ${a} is missing`);
  });
});

describe('managed Node.js', () => {
  /** A "node" that runs the real one, marking the environment so the test can tell. */
  async function fakeNode(name: string, opts: { broken?: boolean } = {}): Promise<string> {
    const file = join(tmp, `${name}.sh`);
    await writeFile(
      file,
      opts.broken
        ? '#!/bin/sh\nexit 9\n'
        : `#!/bin/sh\nFAKE_NODE=${name} exec ${JSON.stringify(process.execPath)} "$@"\n`,
    );
    await chmod(file, 0o755);
    return file;
  }

  it('re-executes the launcher under the managed binary and keeps it once confirmed', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    await activateVersion(root, a);
    await writeLauncher(root);
    const node = await fakeNode('managed');
    await setManagedNode(root, node, Date.now() + 60_000);
    expect(await readInstallState(root)).toMatchObject({
      node,
      pending: { id: a, previous: a, previousNode: null, attempts: 0 },
    });
    const r = launch('agent', 'start');
    expect(r.status).toBe(0);
    const out = JSON.parse(await readFile(r.out, 'utf8')) as Record<string, unknown>;
    expect(out).toMatchObject({
      name: 'a',
      node: 'managed',
      argv: [expect.any(String), 'agent', 'start'],
    });
    // One launch counts once, although the launcher ran twice.
    expect((await readInstallState(root))?.pending?.attempts).toBe(1);
    expect(await confirmPending(root, a)).toBe(true);
    // Activating another version keeps the managed binary.
    const b = await installFromPackageDir(root, await fakePackage('b'));
    await activateVersion(root, b);
    expect((await readInstallState(root))?.node).toBe(node);
  });

  it('goes back to the previous binary when the new one keeps failing', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    await activateVersion(root, a);
    await writeLauncher(root);
    await setManagedNode(root, await fakeNode('broken', { broken: true }), Date.now() + 60_000);
    for (let i = 0; i < LAUNCHER_MAX_ATTEMPTS; i++) expect(launch().status).toBe(9);
    const r = launch();
    expect(r.status).toBe(0);
    expect(r.stderr).toContain('rolled back');
    const out = JSON.parse(await readFile(r.out, 'utf8')) as Record<string, unknown>;
    expect(out).toMatchObject({ name: 'a', node: null });
    const state = await readInstallState(root);
    expect(state).toMatchObject({
      current: a,
      lastRollback: { from: a, to: a, reason: 'crashed' },
    });
    expect(state?.node).toBeUndefined();
  });

  it('combines with a version switch and rewrites older launchers', async () => {
    const a = await installFromPackageDir(root, await fakePackage('a'));
    const b = await installFromPackageDir(root, await fakePackage('b'));
    await activateVersion(root, a);
    await activateVersion(root, b, { deadline: Date.now() + 60_000 });
    const node = await fakeNode('n2');
    await setManagedNode(root, node, Date.now() + 60_000);
    expect(await readInstallState(root)).toMatchObject({
      current: b,
      node,
      pending: { id: b, previous: a, previousNode: null },
    });
    await writeFile(launcherPath(root), '// launcher-version: 1\n');
    expect(await refreshLauncher(root)).toBe(true);
    expect(await refreshLauncher(root)).toBe(false);
    expect(launch().status).toBe(0);
  });
});

import { createHash } from 'node:crypto';
import { createReadStream, existsSync, readFileSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  activateVersion,
  installFromPackageDir,
  packageIdentity,
  packPackage,
  readInstallState,
  versionDir,
  type BuildIdentity,
} from '@gridstator/shared';
import { GridstatorClient } from '@gridstator/client';
import {
  initController,
  issueClientTokenOffline,
  startController,
  type RunningController,
} from './controller.js';

let dir: string;
let data: string;
let root: string;
let adminToken: string;
let ctrl: RunningController | undefined;
let admin: GridstatorClient | undefined;
/** Per test, so a restart scheduled by an earlier test cannot count here. */
let restarts = { n: 0 };

async function fakePackage(name: string): Promise<string> {
  const pkg = join(dir, 'pkgs', name);
  await mkdir(join(pkg, 'dist'), { recursive: true });
  await writeFile(
    join(pkg, 'package.json'),
    JSON.stringify({ name: 'gridstator', version: '0.3.0' }),
  );
  await writeFile(join(pkg, 'dist', 'gridstator.mjs'), `// ${name}\n`);
  return pkg;
}

async function managed(id: string): Promise<BuildIdentity> {
  const packageDir = versionDir(root, id);
  return { ...(await packageIdentity(packageDir)), packageDir, install: { root, id } };
}

async function start(build: BuildIdentity, withRestart = true): Promise<void> {
  const counter = restarts;
  ctrl = await startController({
    dataDir: data,
    host: '127.0.0.1',
    port: 0,
    build,
    ...(withRestart ? { onRestart: () => void counter.n++ } : {}),
    toolchainFetcher: () => Promise.reject(new Error('no network in tests')),
  });
  admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: adminToken });
}

async function stop(): Promise<void> {
  admin?.close();
  await ctrl?.close();
  ctrl = undefined;
  admin = undefined;
}

async function publish(pkgDir: string) {
  const tgz = join(dir, `${Date.now()}.tgz`);
  await packPackage(pkgDir, tgz);
  const sha = createHash('sha256').update(readFileSync(tgz)).digest('hex');
  return admin!.publishBuild(createReadStream(tgz), sha);
}

let idA: string;
let pkgB: string;

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-self-'));
  data = join(dir, 'data');
  root = join(data, 'install');
  adminToken = (await initController({ dataDir: data, host: '127.0.0.1', port: 0 })).adminToken;
  idA = await installFromPackageDir(root, await fakePackage('a'));
  await activateVersion(root, idA);
  pkgB = await fakePackage('b');
  restarts = { n: 0 };
});

afterEach(async () => {
  await stop();
  await rm(dir, { recursive: true, force: true });
});

describe('controller self-update', () => {
  it('switches to a published build provisionally and confirms it after the restart', async () => {
    await start(await managed(idA));
    const res = await publish(pkgB);
    const idB = (await packageIdentity(pkgB)).id;
    expect(res).toEqual({ action: 'restarting', from: idA, to: idB });
    await new Promise((r) => setTimeout(r, 500));
    expect(restarts.n).toBe(1);
    expect(await readInstallState(root)).toMatchObject({
      current: idB,
      previous: idA,
      pending: { id: idB, previous: idA },
    });
    expect(existsSync(join(data, 'updates', `db-before-${idB}.sqlite`))).toBe(true);
    await expect(publish(pkgB)).rejects.toThrow(/already switching/);
    expect(restarts.n).toBe(1);

    // The service manager starts the new version, which confirms itself.
    await stop();
    await start(await managed(idB));
    expect((await readInstallState(root))?.pending).toBeUndefined();
    expect(existsSync(join(data, 'updates', 'switch.json'))).toBe(false);
    expect((await admin!.cluster()).build?.id).toBe(idB);
    expect(await publish(pkgB)).toMatchObject({ action: 'none' });
  });

  it('restores the database from before the switch when the launcher rolls back', async () => {
    await start(await managed(idA));
    const idB = (await publish(pkgB)).to;
    // The new version writes to (e.g. migrates) the database, then fails.
    const late = await admin!.createToken({ kind: 'client', name: 'made-after-snapshot' });
    await stop();
    const state = JSON.parse(await readFile(join(root, 'state.json'), 'utf8')) as Record<
      string,
      unknown
    >;
    await writeFile(
      join(root, 'state.json'),
      JSON.stringify({
        version: 1,
        current: idA,
        lastRollback: { from: idB, to: idA, at: Date.now(), reason: 'crashed' },
        ...(state['node'] ? { node: state['node'] } : {}),
      }),
    );
    await start(await managed(idA));
    const tokens = await admin!.listTokens();
    expect(tokens.map((t) => t.name)).not.toContain('made-after-snapshot');
    expect(late.token).toBeTruthy();
    expect(existsSync(join(data, `controller.db.failed-${idB}`))).toBe(true);
    expect(existsSync(join(data, 'updates', 'switch.json'))).toBe(false);
  });

  it('refuses builds it cannot switch to', async () => {
    await start({ ...(await managed(idA)), install: null });
    await expect(publish(pkgB)).rejects.toThrow(/not a managed install/);
    await stop();
    await start(await managed(idA), false);
    await expect(publish(pkgB)).rejects.toThrow(/cannot restart itself/);
    await stop();
    await start(await managed(idA));
    const tgz = join(dir, 'b.tgz');
    await packPackage(pkgB, tgz);
    await expect(admin!.publishBuild(createReadStream(tgz), '0'.repeat(64))).rejects.toThrow(
      /expected 0000/,
    );
    expect(restarts.n).toBe(0);
  });

  it('issues client tokens offline for recovery', async () => {
    await start(await managed(idA));
    const issued = await issueClientTokenOffline({
      dataDir: data,
      name: 'recovered',
      role: 'admin',
    });
    expect(issued.url).toMatch(/^https:\/\/127\.0\.0\.1:/);
    const client = new GridstatorClient({ url: ctrl!.url, ca: issued.caPem, token: issued.token });
    try {
      expect((await client.cluster()).whoami).toEqual({ name: 'recovered', role: 'admin' });
    } finally {
      client.close();
    }
  });
});

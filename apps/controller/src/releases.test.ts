import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { GridstatorClient } from '@gridstator/client';
import {
  activateVersion,
  installFromPackageDir,
  machineIdentity,
  packageIdentity,
  packPackage,
  readInstallState,
  versionDir,
} from '@gridstator/shared';
import { initController, startController, type RunningController } from './controller.js';

const API = 'https://api.github.com/repos/jepsenphil/gridstator/releases';
const ASSETS = 'https://github.com/jepsenphil/gridstator/releases/download/v0.2.0';
let dir: string;
let ctrl: RunningController | undefined;
let admin: GridstatorClient | undefined;
let root: string;
let initialId: string;
let packageBytes: Buffer;
let targetId: string;
let downloads: Map<string, string | Buffer>;
let fetched: string[];

function release(tag = 'v0.2.0', overrides: Record<string, unknown> = {}) {
  return {
    tag_name: tag,
    name: `Gridstator ${tag}`,
    body: 'A release with fixes.',
    published_at: '2026-10-01T00:00:00Z',
    draft: false,
    prerelease: false,
    assets: [{ name: `gridstator-${tag.slice(1)}.tgz` }, { name: 'SHA256SUMS' }],
    ...overrides,
  };
}

async function fakePackage(name: string, version: string): Promise<string> {
  const path = join(dir, name);
  await mkdir(join(path, 'dist'), { recursive: true });
  await writeFile(join(path, 'package.json'), JSON.stringify({ name: 'gridstator', version }));
  await writeFile(join(path, 'dist', 'gridstator.mjs'), `// ${name}\n`);
  return path;
}

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-releases-'));
  const dataDir = join(dir, 'data');
  root = join(dataDir, 'install');
  const init = await initController({ dataDir });
  initialId = await installFromPackageDir(root, await fakePackage('current', '0.1.0'));
  await activateVersion(root, initialId);
  const target = await fakePackage('target', '0.2.0');
  targetId = (await packageIdentity(target)).id;
  const tarball = join(dir, 'target.tgz');
  await packPackage(target, tarball);
  packageBytes = await readFile(tarball);
  const digest = createHash('sha256').update(packageBytes).digest('hex');
  downloads = new Map([
    [`${API}?per_page=100`, JSON.stringify([release()])],
    [`${API}/tags/v0.2.0`, JSON.stringify(release())],
    [`${ASSETS}/SHA256SUMS`, `${digest}  gridstator-0.2.0.tgz\n`],
  ]);
  downloads.set(`${ASSETS}/gridstator-0.2.0.tgz`, packageBytes);
  fetched = [];
  const packageDir = versionDir(root, initialId);
  ctrl = await startController({
    dataDir,
    host: '127.0.0.1',
    port: 0,
    build: { ...(await packageIdentity(packageDir)), packageDir, install: { root, id: initialId } },
    onRestart: () => undefined,
    toolchainFetcher: () => Promise.reject(new Error('No toolchain network in release tests')),
    releaseFetcher: async (url) => {
      fetched.push(url);
      const body = downloads.get(url);
      if (body === undefined) throw new Error(`Unexpected download ${url}`);
      return Readable.from([body]);
    },
  });
  admin = new GridstatorClient({ url: ctrl.url, ca: ctrl.caPem, token: init.adminToken });
});

afterEach(async () => {
  admin?.close();
  await ctrl?.close();
  ctrl = undefined;
  admin = undefined;
  await rm(dir, { recursive: true, force: true });
});

describe('official controller releases', () => {
  it('reports controller identity and offers stable published packages, with prereleases opt-in', async () => {
    downloads.set(
      `${API}?per_page=100`,
      JSON.stringify([
        release(),
        release('v0.3.0-beta.1', { prerelease: true }),
        release('v0.3.0', { draft: true }),
        release('v0.0.1', { assets: [] }),
        release('not-a-version'),
      ]),
    );
    expect((await admin!.cluster()).controller).toMatchObject({
      machineId: await machineIdentity(),
    });
    expect((await admin!.cluster()).build?.selfUpdate).toEqual({ available: true, reason: null });
    expect((await admin!.updates()).controller.selfUpdate).toEqual({
      available: true,
      reason: null,
    });
    const stable = await admin!.listReleases();
    expect(stable.map((item) => item.tag)).toEqual(['v0.2.0']);
    expect(stable[0]).toMatchObject({
      version: '0.2.0',
      url: 'https://github.com/jepsenphil/gridstator/releases/tag/v0.2.0',
      packageName: 'gridstator-0.2.0.tgz',
    });
    expect((await admin!.listReleases(true)).map((item) => item.tag)).toEqual([
      'v0.2.0',
      'v0.3.0-beta.1',
    ]);
    expect(fetched).toEqual([`${API}?per_page=100`]);
  });

  it('downloads the selected official package, verifies its checksum and switches managed builds', async () => {
    const response = await admin!.installRelease('v0.2.0');
    expect(response).toEqual({ action: 'restarting', from: initialId, to: targetId });
    expect((await readInstallState(root))?.current).toBe(targetId);
    expect(fetched).toEqual([
      `${API}/tags/v0.2.0`,
      `${ASSETS}/SHA256SUMS`,
      `${ASSETS}/gridstator-0.2.0.tgz`,
    ]);
  });

  it('rejects a corrupt download before changing the controller installation', async () => {
    downloads.set(`${ASSETS}/gridstator-0.2.0.tgz`, Buffer.from('corrupt package'));
    await expect(admin!.installRelease('v0.2.0')).rejects.toThrow(/sha256.*expected/);
    expect((await readInstallState(root))?.current).toBe(initialId);
    expect((await readInstallState(root))?.pending).toBeUndefined();
  });

  it('refuses missing or ambiguous checksums before downloading a package', async () => {
    downloads.set(
      `${ASSETS}/SHA256SUMS`,
      `${'a'.repeat(64)}  gridstator-0.2.0.tgz\n${'b'.repeat(64)}  gridstator-0.2.0.tgz\n`,
    );
    await expect(admin!.installRelease('v0.2.0')).rejects.toThrow(/published checksums/);
    expect(fetched).not.toContain(`${ASSETS}/gridstator-0.2.0.tgz`);
    expect((await readInstallState(root))?.current).toBe(initialId);
  });

  it('allows an operator to view releases but requires admin access to install', async () => {
    const token = await admin!.createToken({ kind: 'client', name: 'operator' });
    const operator = new GridstatorClient({ url: ctrl!.url, ca: ctrl!.caPem, token: token.token });
    try {
      await expect(operator.listReleases()).resolves.toHaveLength(1);
      fetched.length = 0;
      await expect(operator.installRelease('v0.2.0')).rejects.toThrow(/admin token/);
      expect(fetched).toEqual([]);
    } finally {
      operator.close();
    }
  });

  it('reports release discovery failures without altering the installation', async () => {
    downloads.delete(`${API}?per_page=100`);
    await expect(admin!.listReleases()).rejects.toThrow(/Could not check Gridstator releases/);
    expect((await readInstallState(root))?.current).toBe(initialId);
  });

  it('rejects arbitrary release identifiers without making an outbound request', async () => {
    await expect(admin!.installRelease('https://example.com/package')).rejects.toThrow(
      /published Gridstator release tag/,
    );
    expect(fetched).toEqual([]);
  });
});

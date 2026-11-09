import { execFileSync } from 'node:child_process';
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readlink,
  rm,
  symlink,
  writeFile,
  chmod,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import tar from 'tar-stream';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { parseManifest, type Manifest } from '@gridstator/protocol';
import {
  BlobStore,
  captureSnapshot,
  digestOf,
  findProject,
  loadProjectConfig,
  materializeSnapshot,
  packBlobs,
  unpackBlobsInto,
  uploadSnapshot,
  workspaceTar,
  type SnapshotTransport,
} from './index.js';

let dir: string;

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-snap-'));
});
afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

function git(cwd: string, ...args: string[]): void {
  execFileSync('git', ['-c', 'user.email=t@example.com', '-c', 'user.name=t', ...args], {
    cwd,
    stdio: 'ignore',
  });
}

async function write(rel: string, content: string): Promise<void> {
  await mkdir(join(dir, rel, '..'), { recursive: true });
  await writeFile(join(dir, rel), content);
}

async function makeRepo(): Promise<string> {
  const repo = join(dir, 'repo');
  await mkdir(repo);
  git(repo, 'init', '-q');
  const w = async (rel: string, content: string) => {
    await mkdir(join(repo, rel, '..'), { recursive: true });
    await writeFile(join(repo, rel), content);
  };
  await w('.gitignore', 'dist/\n*.log\n');
  await w('src/index.ts', 'export const v = 1;\n');
  await w('src/index.test.ts', 'test\n');
  await w('.env', 'SECRET=1\n');
  await w('.secrets', 'GITHUB_TOKEN=x\n');
  await w('.env.example', 'SECRET=\n');
  await w('certs/server.pem', 'pem\n');
  await w('.npmrc', '//registry.npmjs.org/:_authToken=abc\n');
  await w('packages/a/.npmrc', 'save-exact=true\n');
  await w('node_modules/x/index.js', 'x\n');
  await w('run.sh', '#!/bin/sh\necho hi\n');
  await chmod(join(repo, 'run.sh'), 0o755);
  git(
    repo,
    'add',
    '-A',
    '-f',
    '.gitignore',
    'src',
    '.env',
    '.env.example',
    'certs',
    '.npmrc',
    'packages',
    'run.sh',
  );
  git(repo, 'commit', '-q', '-m', 'init');
  // Untracked, ignored, and modified-but-uncommitted files.
  await w('src/new-file.ts', 'untracked\n');
  await w('dist/out.js', 'ignored\n');
  await w('debug.log', 'ignored\n');
  await w('src/index.ts', 'export const v = 2; // uncommitted change\n');
  await symlink('src/index.ts', join(repo, 'link-inside'));
  await symlink('/etc/passwd', join(repo, 'link-outside'));
  return repo;
}

function paths(m: Manifest): string[] {
  return m.entries.map((e) => e.path);
}

describe('captureSnapshot (git)', () => {
  it('includes tracked + untracked files, uses working-tree content, excludes ignored/secrets/node_modules', async () => {
    const repo = await makeRepo();
    const snap = await captureSnapshot({ root: repo });
    expect(snap.stats.source).toBe('git');
    expect(paths(snap.manifest)).toEqual([
      '.env.example',
      '.gitignore',
      'link-inside',
      'packages/a/.npmrc',
      'run.sh',
      'src/index.test.ts',
      'src/index.ts',
      'src/new-file.ts',
    ]);
    const index = snap.manifest.entries.find((e) => e.path === 'src/index.ts');
    expect(index).toMatchObject({
      type: 'file',
      digest: digestOf('export const v = 2; // uncommitted change\n'),
    });
    expect(snap.manifest.entries.find((e) => e.path === 'run.sh')).toMatchObject({
      executable: true,
    });
    expect(snap.manifest.entries.find((e) => e.path === 'link-inside')).toEqual({
      path: 'link-inside',
      type: 'symlink',
      target: 'src/index.ts',
    });
    const warned = snap.warnings.map((w) => `${w.kind}:${w.path}`);
    expect(warned).toEqual([
      'secret:.env',
      'secret:.npmrc',
      'secret:.secrets',
      'secret:certs/server.pem',
      'symlink:link-outside',
    ]);
  });

  it('produces a stable id for identical content and a new id for any change', async () => {
    const repo = await makeRepo();
    const a = await captureSnapshot({ root: repo });
    const b = await captureSnapshot({ root: repo });
    expect(b.id).toBe(a.id);
    expect(a.id).toBe(digestOf(a.manifestBytes));
    await writeFile(join(repo, 'src/index.ts'), 'export const v = 3;\n');
    const c = await captureSnapshot({ root: repo });
    expect(c.id).not.toBe(a.id);
  });

  it('honours include, exclude and allowSecrets', async () => {
    const repo = await makeRepo();
    const snap = await captureSnapshot({
      root: repo,
      include: ['dist/**'],
      exclude: ['src/**/*.test.ts'],
      allowSecrets: ['.env'],
    });
    const p = paths(snap.manifest);
    expect(p).toContain('dist/out.js');
    expect(p).toContain('.env');
    expect(p).not.toContain('src/index.test.ts');
    expect(p).not.toContain('debug.log');
  });

  it('uses the hash cache without changing results', async () => {
    const repo = await makeRepo();
    const cacheDir = join(dir, 'cache');
    // Make files old enough to be cacheable.
    const old = new Date(Date.now() - 60_000);
    const { utimes } = await import('node:fs/promises');
    for (const f of ['src/index.ts', 'src/new-file.ts', 'run.sh'])
      await utimes(join(repo, f), old, old);
    const first = await captureSnapshot({ root: repo, cacheDir });
    const second = await captureSnapshot({ root: repo, cacheDir });
    expect(second.id).toBe(first.id);
    expect(second.stats.cachedFiles).toBeGreaterThan(0);
  });

  it('excludes files above maxFileSize with a warning', async () => {
    const repo = await makeRepo();
    await writeFile(join(repo, 'big.bin'), Buffer.alloc(2048));
    const snap = await captureSnapshot({ root: repo, maxFileSize: 1024 });
    expect(paths(snap.manifest)).not.toContain('big.bin');
    expect(snap.warnings.some((w) => w.kind === 'too-large' && w.path === 'big.bin')).toBe(true);
  });
});

describe('captureSnapshot (no git)', () => {
  it('walks the tree applying .gitignore files and builtin excludes', async () => {
    await write('a.txt', 'a');
    await write('.gitignore', 'ignored/\n');
    await write('ignored/x.txt', 'x');
    await write('sub/.gitignore', '*.tmp\n');
    await write('sub/keep.txt', 'k');
    await write('sub/drop.tmp', 'd');
    await write('node_modules/m.js', 'm');
    await write('sub/node_modules/m.js', 'm');
    const snap = await captureSnapshot({ root: dir });
    expect(snap.stats.source).toBe('walk');
    expect(paths(snap.manifest)).toEqual(['.gitignore', 'a.txt', 'sub/.gitignore', 'sub/keep.txt']);
  });
});

describe('BlobStore', () => {
  it('stores verified content and rejects mismatches', async () => {
    const store = new BlobStore(join(dir, 'cas'));
    await store.init();
    const data = Buffer.from('hello');
    const d = digestOf(data);
    expect(await store.putBuffer(d, data)).toBe(true);
    expect(await store.putBuffer(d, data)).toBe(false);
    expect((await store.readBuffer(d)).toString()).toBe('hello');
    await expect(store.putBuffer(digestOf('other'), data)).rejects.toThrow(/does not match/);
    expect(await store.missing([d, digestOf('nope')])).toEqual([digestOf('nope')]);
  });

  it('round-trips blobs through a tar stream', async () => {
    const src = new BlobStore(join(dir, 'src'));
    const dst = new BlobStore(join(dir, 'dst'));
    await src.init();
    await dst.init();
    const blobs = ['one', 'two', 'x'.repeat(200_000)].map((s) => Buffer.from(s));
    for (const b of blobs) await src.putBuffer(digestOf(b), b);
    const tarStream = packBlobs(
      blobs.map((b) => ({
        digest: digestOf(b),
        size: b.length,
        open: () => src.read(digestOf(b)),
      })),
    );
    const result = await unpackBlobsInto(tarStream, dst);
    expect(result).toEqual({
      stored: 3,
      alreadyPresent: 0,
      bytes: blobs.reduce((n, b) => n + b.length, 0),
    });
    for (const b of blobs) expect(await dst.readBuffer(digestOf(b))).toEqual(b);
  });

  it('rejects unexpected entries in a blob tar', async () => {
    const dst = new BlobStore(join(dir, 'dst'));
    await dst.init();
    const pack = tar.pack();
    pack.entry({ name: '../evil' }, 'x');
    pack.finalize();
    await expect(
      unpackBlobsInto(Readable.from(pack as AsyncIterable<Buffer>), dst),
    ).rejects.toThrow(/Unexpected entry/);
  });
});

describe('upload + materialize', () => {
  async function uploadToStore(repo: string, store: BlobStore) {
    const snap = await captureSnapshot({ root: repo });
    const registered: string[] = [];
    const transport: SnapshotTransport = {
      missingBlobs: (d) => store.missing(d),
      uploadBlobs: (body) => unpackBlobsInto(body, store),
      createSnapshot: async (id) => {
        registered.push(id);
        parseManifest(await store.readBuffer(id));
        return { id, fileCount: 0, totalBytes: 0, createdAt: 0, lastUsedAt: 0, owner: 't' };
      },
    };
    const result = await uploadSnapshot(snap, transport);
    return { snap, registered, result, transport };
  }

  it('uploads only missing blobs and materializes the exact tree', async () => {
    const repo = await makeRepo();
    const store = new BlobStore(join(dir, 'cas'));
    await store.init();
    const { snap, registered, result } = await uploadToStore(repo, store);
    expect(registered).toEqual([snap.id]);
    expect(result.progress.missing).toBe(snap.blobs.size + 1);

    const again = await uploadToStore(repo, store);
    expect(again.result.progress.missing).toBe(0);

    const out = join(dir, 'out');
    await materializeSnapshot(parseManifest(await store.readBuffer(snap.id)), store, out);
    expect(await readFile(join(out, 'src/index.ts'), 'utf8')).toBe(
      'export const v = 2; // uncommitted change\n',
    );
    expect(await readlink(join(out, 'link-inside'))).toBe('src/index.ts');
    expect((await lstat(join(out, 'run.sh'))).mode & 0o111).not.toBe(0);
    await expect(lstat(join(out, '.env'))).rejects.toThrow();
  });

  it('refuses to materialize into a non-empty directory or with unsafe manifests', async () => {
    const store = new BlobStore(join(dir, 'cas'));
    await store.init();
    const out = join(dir, 'out');
    await mkdir(out);
    await writeFile(join(out, 'x'), 'x');
    await expect(materializeSnapshot({ version: 1, entries: [] }, store, out)).rejects.toThrow(
      /non-empty/,
    );
    const bad = (entries: unknown[]) =>
      materializeSnapshot({ version: 1, entries } as Manifest, store, join(dir, 'o2'));
    const d = digestOf('x');
    await expect(
      bad([{ path: '../escape', type: 'file', size: 1, digest: d, executable: false }]),
    ).rejects.toThrow(/".." path segment/);
    await expect(
      bad([{ path: '/abs', type: 'file', size: 1, digest: d, executable: false }]),
    ).rejects.toThrow(/absolute/);
    await expect(bad([{ path: 'l', type: 'symlink', target: '../../etc' }])).rejects.toThrow(
      /outside/,
    );
    await expect(
      bad([
        { path: 'l', type: 'symlink', target: 'x' },
        { path: 'l/f', type: 'file', size: 1, digest: d, executable: false },
      ]),
    ).rejects.toThrow(/nested under non-directory/);
  });

  it('builds a workspace tar with directories, modes and symlinks', async () => {
    const repo = await makeRepo();
    const store = new BlobStore(join(dir, 'cas'));
    await store.init();
    const { snap } = await uploadToStore(repo, store);
    const extract = tar.extract();
    workspaceTar(snap.manifest, store, { uid: 1000, gid: 1000 }).pipe(extract);
    const seen: string[] = [];
    for await (const entry of extract) {
      seen.push(
        `${entry.header.type}:${entry.header.name}:${(entry.header.mode ?? 0).toString(8)}:${entry.header.uid}`,
      );
      entry.resume();
    }
    expect(seen).toContain('directory:src:755:1000');
    expect(seen).toContain('file:run.sh:755:1000');
    expect(seen).toContain('file:src/index.ts:644:1000');
    expect(seen).toContain('symlink:link-inside:777:1000');
  });
});

describe('project discovery', () => {
  it('finds the git root and relative workdir, or the nearest gridstator.yml', async () => {
    const repo = await makeRepo();
    await mkdir(join(repo, 'packages/a/src'), { recursive: true });
    let loc = await findProject(join(repo, 'packages/a/src'));
    expect(loc).toMatchObject({
      root: repo,
      workdir: 'packages/a/src',
      git: true,
      configPath: undefined,
    });
    await writeFile(
      join(repo, 'packages/a/gridstator.yml'),
      'project: a\ntasks:\n  t:\n    command: echo hi\n',
    );
    loc = await findProject(join(repo, 'packages/a/src'));
    expect(loc).toMatchObject({ root: join(repo, 'packages/a'), workdir: 'src' });
    const config = await loadProjectConfig(loc.configPath);
    expect(config.project).toBe('a');
    expect(config.tasks['t']?.command).toBe('echo hi');
  });

  it('reports invalid config clearly', async () => {
    const p = join(dir, 'gridstator.yml');
    await writeFile(p, 'bogus: 1\n');
    await expect(loadProjectConfig(p)).rejects.toThrow(/Invalid gridstator.yml/);
  });
});

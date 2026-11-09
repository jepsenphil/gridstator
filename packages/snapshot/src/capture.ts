import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, type Stats } from 'node:fs';
import { lstat, readdir, readFile, readlink } from 'node:fs/promises';
import { join, relative, sep } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { promisify } from 'node:util';
import ignore, { type Ignore } from 'ignore';
import picomatch from 'picomatch';
import { GridstatorError, readJsonFile, sha256Hex, writeJsonFile } from '@gridstator/shared';
import {
  MANIFEST_VERSION,
  checkSnapshotPath,
  comparePaths,
  resolveSymlinkTarget,
  serializeManifest,
  validateManifest,
  type Manifest,
  type ManifestEntry,
} from '@gridstator/protocol';
import { digestOf } from './cas.js';
import { createSecretFilter } from './secrets.js';

const execFileAsync = promisify(execFile);

/** Always excluded, regardless of git or configuration. */
export const BUILTIN_EXCLUDES: readonly string[] = [
  '**/.git',
  '**/.git/**',
  '**/node_modules',
  '**/node_modules/**',
];

export interface CaptureOptions {
  root: string;
  include?: readonly string[];
  exclude?: readonly string[];
  allowSecrets?: readonly string[];
  maxFileSize?: number;
  /** Directory for the stat/hash cache. Omit to disable caching. */
  cacheDir?: string;
  /** Force the non-git walker even inside a git repository. */
  noGit?: boolean;
}

export type CaptureWarningKind = 'secret' | 'too-large' | 'symlink' | 'unsupported' | 'unsafe-path';

export interface CaptureWarning {
  kind: CaptureWarningKind;
  path: string;
  message: string;
}

export interface CapturedFile {
  absPath: string;
  size: number;
}

export interface CapturedSnapshot {
  id: string;
  root: string;
  manifest: Manifest;
  manifestBytes: Buffer;
  /** One local source file per distinct content digest. */
  blobs: Map<string, CapturedFile>;
  warnings: CaptureWarning[];
  stats: {
    files: number;
    symlinks: number;
    totalBytes: number;
    hashedFiles: number;
    cachedFiles: number;
    durationMs: number;
    source: 'git' | 'walk';
  };
}

interface HashCacheFile {
  version: 1;
  entries: Record<
    string,
    [size: number, mtimeMs: number, ctimeMs: number, ino: number, digest: string]
  >;
}

async function listGitFiles(root: string): Promise<string[]> {
  const { stdout } = await execFileAsync(
    'git',
    [
      '-C',
      root,
      '-c',
      'core.quotePath=false',
      'ls-files',
      '-z',
      '--cached',
      '--others',
      '--exclude-standard',
    ],
    { encoding: 'buffer', maxBuffer: 1024 * 1024 * 1024 },
  );
  const paths = stdout
    .toString('utf8')
    .split('\0')
    .filter((p) => p !== '');
  const unique = [...new Set(paths)];
  const result: string[] = [];
  for (const p of unique) {
    // Submodules appear as a single gitlink path that is a directory: recurse into them.
    let st: Stats;
    try {
      st = await lstat(join(root, p));
    } catch {
      continue; // tracked but deleted in the working tree
    }
    if (st.isDirectory()) {
      try {
        await lstat(join(root, p, '.git'));
        const nested = await listGitFiles(join(root, p));
        result.push(...nested.map((n) => `${p}/${n}`));
      } catch {
        // a directory listed by git without .git (should not happen); ignore
      }
      continue;
    }
    result.push(p);
  }
  return result;
}

async function walkFiles(
  root: string,
  shouldDescend: (rel: string) => boolean,
  useGitignore: boolean,
): Promise<string[]> {
  const out: string[] = [];
  const stack: Array<{ dir: string; rules: Array<{ base: string; ig: Ignore }> }> = [
    { dir: '', rules: [] },
  ];
  while (stack.length > 0) {
    const { dir, rules: parentRules } = stack.pop()!;
    const absDir = join(root, dir);
    let rules = parentRules;
    if (useGitignore) {
      try {
        const content = await readFile(join(absDir, '.gitignore'), 'utf8');
        rules = [...parentRules, { base: dir, ig: ignore().add(content) }];
      } catch {
        // no .gitignore here
      }
    }
    const ignored = (rel: string, isDir: boolean): boolean =>
      rules.some(({ base, ig }) => {
        const sub = base === '' ? rel : rel.slice(base.length + 1);
        return ig.ignores(isDir ? `${sub}/` : sub);
      });
    let names: string[];
    try {
      names = await readdir(absDir);
    } catch {
      continue;
    }
    for (const name of names) {
      const rel = dir === '' ? name : `${dir}/${name}`;
      let st: Stats;
      try {
        st = await lstat(join(root, rel));
      } catch {
        continue;
      }
      if (st.isDirectory()) {
        if (name === '.git' || !shouldDescend(rel) || ignored(rel, true)) continue;
        stack.push({ dir: rel, rules });
      } else if (!ignored(rel, false)) {
        out.push(rel);
      }
    }
  }
  return out;
}

async function isGitWorkTree(root: string): Promise<boolean> {
  try {
    const { stdout } = await execFileAsync(
      'git',
      ['-C', root, 'rev-parse', '--is-inside-work-tree'],
      {
        encoding: 'utf8',
      },
    );
    return stdout.trim() === 'true';
  } catch {
    return false;
  }
}

async function hashFile(path: string, size: number): Promise<string> {
  if (size <= 1024 * 1024) return digestOf(await readFile(path));
  const hash = createHash('sha256');
  await pipeline(createReadStream(path), hash);
  return 'sha256:' + hash.digest('hex');
}

function sameStat(a: Stats, b: Stats): boolean {
  return a.size === b.size && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs && a.ino === b.ino;
}

async function mapLimit<T, R>(
  items: readonly T[],
  limit: number,
  fn: (item: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const i = next++;
      results[i] = await fn(items[i]!);
    }
  });
  await Promise.all(workers);
  return results;
}

/**
 * Captures the working tree at `root` into a content-addressed manifest. Nothing is uploaded; the
 * result lists the local files backing each blob so they can be transferred.
 */
export async function captureSnapshot(options: CaptureOptions): Promise<CapturedSnapshot> {
  const started = Date.now();
  const root = options.root;
  const warnings: CaptureWarning[] = [];
  const isExcluded = picomatch([...BUILTIN_EXCLUDES, ...(options.exclude ?? [])], { dot: true });
  const secretFilter = createSecretFilter(options.allowSecrets ?? []);
  const maxFileSize = options.maxFileSize ?? 256 * 1024 * 1024;

  const useGit = !options.noGit && (await isGitWorkTree(root));
  const candidates = new Set<string>(
    useGit ? await listGitFiles(root) : await walkFiles(root, (rel) => !isExcluded(rel), true),
  );

  if (options.include && options.include.length > 0) {
    const isIncluded = picomatch([...options.include], { dot: true });
    const bases = new Set(options.include.map((p) => picomatch.scan(p).base || ''));
    for (const base of bases) {
      const baseProblem = base === '' ? undefined : checkSnapshotPath(base);
      if (baseProblem) continue;
      const found = await walkFiles(
        join(root, base),
        (rel) => !rel.split('/').includes('.git'),
        false,
      );
      for (const f of found) {
        const rel = base === '' ? f : `${base}/${f}`;
        if (isIncluded(rel)) candidates.add(rel);
      }
    }
  }

  const cachePath = options.cacheDir
    ? join(options.cacheDir, 'hashcache', `${sha256Hex(root).slice(0, 32)}.json`)
    : undefined;
  const cacheFile = cachePath
    ? await readJsonFile<HashCacheFile>(cachePath).catch(() => undefined)
    : undefined;
  const oldCache = cacheFile?.version === 1 ? cacheFile.entries : {};
  const newCache: HashCacheFile['entries'] = {};
  let hashedFiles = 0;
  let cachedFiles = 0;

  const paths = [...candidates].map((p) => p.split(sep).join('/')).sort(comparePaths);
  const entries = await mapLimit(paths, 16, async (rel): Promise<ManifestEntry | undefined> => {
    const problem = checkSnapshotPath(rel);
    if (problem) {
      warnings.push({ kind: 'unsafe-path', path: rel, message: `skipped: ${problem}` });
      return undefined;
    }
    if (isExcluded(rel)) return undefined;
    const abs = join(root, rel);
    for (let attempt = 0; attempt < 5; attempt++) {
      let before: Stats;
      try {
        before = await lstat(abs);
      } catch {
        return undefined; // deleted while capturing
      }
      if (before.isSymbolicLink()) {
        const target = await readlink(abs);
        if (resolveSymlinkTarget(rel, target) === undefined) {
          warnings.push({
            kind: 'symlink',
            path: rel,
            message: `skipped: symlink target "${target}" is outside the project`,
          });
          return undefined;
        }
        return { path: rel, type: 'symlink', target };
      }
      if (!before.isFile()) {
        warnings.push({ kind: 'unsupported', path: rel, message: 'skipped: not a regular file' });
        return undefined;
      }
      const secret = await secretFilter.check(rel, () => readFile(abs, 'utf8'));
      if (secret) {
        warnings.push({ kind: 'secret', path: rel, message: `excluded: ${secret}` });
        return undefined;
      }
      if (before.size > maxFileSize) {
        warnings.push({
          kind: 'too-large',
          path: rel,
          message: `excluded: ${before.size} bytes exceeds snapshot.maxFileSize`,
        });
        return undefined;
      }
      const cached = oldCache[rel];
      let digest: string;
      if (
        cached &&
        cached[0] === before.size &&
        cached[1] === before.mtimeMs &&
        cached[2] === before.ctimeMs &&
        cached[3] === before.ino
      ) {
        digest = cached[4];
        cachedFiles++;
      } else {
        digest = await hashFile(abs, before.size);
        hashedFiles++;
      }
      const after = await lstat(abs).catch(() => undefined);
      if (!after || !sameStat(before, after)) continue; // changed while hashing: retry
      // Only cache entries old enough that a same-timestamp rewrite is implausible.
      if (Date.now() - before.mtimeMs > 2000) {
        newCache[rel] = [before.size, before.mtimeMs, before.ctimeMs, before.ino, digest];
      }
      return {
        path: rel,
        type: 'file',
        size: before.size,
        digest,
        executable: process.platform !== 'win32' && (before.mode & 0o111) !== 0,
      };
    }
    throw new GridstatorError(
      'CONFLICT',
      `File "${rel}" kept changing while the snapshot was captured`,
      {
        hint: 'Wait for builds or file watchers writing to it to finish, or exclude it in gridstator.yml.',
      },
    );
  });

  const manifestEntries = entries.filter((e): e is ManifestEntry => e !== undefined);
  // Drop entries nested under a file/symlink entry (e.g. a symlinked directory listed by git).
  const leaves = new Set(manifestEntries.map((e) => e.path));
  const finalEntries = manifestEntries.filter((e) => {
    const parts = e.path.split('/');
    for (let i = 1; i < parts.length; i++) {
      if (leaves.has(parts.slice(0, i).join('/'))) return false;
    }
    return true;
  });

  const manifest: Manifest = { version: MANIFEST_VERSION, entries: finalEntries };
  validateManifest(manifest);
  const manifestBytes = Buffer.from(serializeManifest(manifest), 'utf8');
  const id = digestOf(manifestBytes);

  const blobs = new Map<string, CapturedFile>();
  let totalBytes = 0;
  let files = 0;
  let symlinks = 0;
  for (const e of finalEntries) {
    if (e.type === 'symlink') {
      symlinks++;
      continue;
    }
    files++;
    totalBytes += e.size;
    if (!blobs.has(e.digest)) blobs.set(e.digest, { absPath: join(root, e.path), size: e.size });
  }

  if (cachePath) {
    await writeJsonFile(cachePath, { version: 1, entries: newCache } satisfies HashCacheFile).catch(
      () => undefined,
    );
  }

  warnings.sort((a, b) => comparePaths(a.path, b.path));
  return {
    id,
    root,
    manifest,
    manifestBytes,
    blobs,
    warnings,
    stats: {
      files,
      symlinks,
      totalBytes,
      hashedFiles,
      cachedFiles,
      durationMs: Date.now() - started,
      source: useGit ? 'git' : 'walk',
    },
  };
}

/** Converts an absolute path inside `root` to a snapshot-relative POSIX path. */
export function toSnapshotPath(root: string, abs: string): string {
  return relative(root, abs).split(sep).join('/');
}

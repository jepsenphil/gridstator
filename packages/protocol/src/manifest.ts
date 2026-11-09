import { posix } from 'node:path';
import { z } from 'zod';
import { DigestSchema } from './common.js';

export const MANIFEST_VERSION = 1;
export const MAX_SNAPSHOT_PATH_LENGTH = 4096;

export const FileEntrySchema = z.object({
  path: z.string(),
  type: z.literal('file'),
  size: z.number().int().nonnegative(),
  digest: DigestSchema,
  executable: z.boolean(),
});
export type FileEntry = z.infer<typeof FileEntrySchema>;

export const SymlinkEntrySchema = z.object({
  path: z.string(),
  type: z.literal('symlink'),
  target: z.string().min(1).max(MAX_SNAPSHOT_PATH_LENGTH),
});
export type SymlinkEntry = z.infer<typeof SymlinkEntrySchema>;

export const ManifestEntrySchema = z.discriminatedUnion('type', [
  FileEntrySchema,
  SymlinkEntrySchema,
]);
export type ManifestEntry = z.infer<typeof ManifestEntrySchema>;

export const ManifestSchema = z.object({
  version: z.literal(MANIFEST_VERSION),
  entries: z.array(ManifestEntrySchema).max(1_000_000),
});
export type Manifest = z.infer<typeof ManifestSchema>;

/**
 * Returns an error message if `p` is not a safe, normalized, relative snapshot path, otherwise
 * undefined. Safe paths cannot escape the extraction root on any supported platform.
 */
export function checkSnapshotPath(p: string): string | undefined {
  if (p.length === 0) return 'empty path';
  if (p.length > MAX_SNAPSHOT_PATH_LENGTH) return 'path too long';
  if (p.includes('\0')) return 'path contains NUL';
  if (p.includes('\\')) return 'path contains a backslash';
  if (p.startsWith('/')) return 'absolute path';
  if (/^[A-Za-z]:/.test(p)) return 'drive-letter path';
  for (const segment of p.split('/')) {
    if (segment === '') return 'empty path segment';
    if (segment === '.' || segment === '..') return `"${segment}" path segment`;
  }
  return undefined;
}

/**
 * Resolves a symlink's target relative to the snapshot root. Returns undefined when the target is
 * absolute or would resolve outside the root.
 */
export function resolveSymlinkTarget(linkPath: string, target: string): string | undefined {
  if (target.startsWith('/') || target.includes('\\') || /^[A-Za-z]:/.test(target)) {
    return undefined;
  }
  if (target.includes('\0')) return undefined;
  const resolved = posix.normalize(posix.join(posix.dirname(linkPath), target));
  if (resolved === '..' || resolved.startsWith('../') || resolved.startsWith('/')) return undefined;
  return resolved;
}

/** Byte-wise comparison (UTF-16 code unit order matches UTF-8 byte order for BMP-only paths). */
export function comparePaths(a: string, b: string): number {
  const ab = Buffer.from(a, 'utf8');
  const bb = Buffer.from(b, 'utf8');
  return Buffer.compare(ab, bb);
}

/**
 * Full structural validation of a manifest: safe paths, strictly sorted (which also rules out
 * duplicates), no entry nested beneath a file or symlink, and symlinks that stay inside the root.
 * Throws an Error describing the first problem.
 */
export function validateManifest(manifest: Manifest): void {
  const leafPaths = new Set<string>();
  let previous: string | undefined;
  for (const entry of manifest.entries) {
    const problem = checkSnapshotPath(entry.path);
    if (problem) throw new Error(`Invalid manifest path "${entry.path}": ${problem}`);
    if (previous !== undefined && comparePaths(previous, entry.path) >= 0) {
      throw new Error(`Manifest entries are not strictly sorted at "${entry.path}"`);
    }
    previous = entry.path;
    if (entry.type === 'symlink' && resolveSymlinkTarget(entry.path, entry.target) === undefined) {
      throw new Error(`Symlink "${entry.path}" points outside the snapshot`);
    }
    leafPaths.add(entry.path);
  }
  for (const entry of manifest.entries) {
    const parts = entry.path.split('/');
    for (let i = 1; i < parts.length; i++) {
      const ancestor = parts.slice(0, i).join('/');
      if (leafPaths.has(ancestor)) {
        throw new Error(
          `Manifest path "${entry.path}" is nested under non-directory "${ancestor}"`,
        );
      }
    }
  }
}

/**
 * Canonical serialization: fixed key order, no whitespace. The snapshot ID is the SHA-256 of these
 * bytes, so identical trees always produce identical IDs.
 */
export function serializeManifest(manifest: Manifest): string {
  const entries = manifest.entries.map((e) =>
    e.type === 'file'
      ? { path: e.path, type: e.type, size: e.size, digest: e.digest, executable: e.executable }
      : { path: e.path, type: e.type, target: e.target },
  );
  return JSON.stringify({ version: manifest.version, entries });
}

export function parseManifest(bytes: string | Uint8Array): Manifest {
  const text = typeof bytes === 'string' ? bytes : Buffer.from(bytes).toString('utf8');
  const manifest = ManifestSchema.parse(JSON.parse(text));
  validateManifest(manifest);
  return manifest;
}

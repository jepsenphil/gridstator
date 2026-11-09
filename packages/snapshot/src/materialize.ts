import { constants } from 'node:fs';
import { mkdir, open, readdir, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { GridstatorError } from '@gridstator/shared';
import { validateManifest, type Manifest } from '@gridstator/protocol';
import type { BlobStore } from './cas.js';

/**
 * Writes a snapshot's working tree into `dest`, which must not exist or be empty.
 *
 * Safety: the manifest is validated (no "..", no absolute paths, no entries beneath symlinks, no
 * symlinks pointing outside the tree); directories are created by us; files are opened with
 * O_CREAT|O_EXCL|O_NOFOLLOW so nothing pre-existing is overwritten or followed; symlinks are
 * created last so no write can traverse one.
 */
export async function materializeSnapshot(
  manifest: Manifest,
  store: BlobStore,
  dest: string,
): Promise<void> {
  validateManifest(manifest);
  await mkdir(dest, { recursive: true, mode: 0o755 });
  const existing = await readdir(dest);
  if (existing.length > 0) {
    throw new GridstatorError(
      'CONFLICT',
      `Refusing to materialize snapshot into non-empty directory ${dest}`,
    );
  }
  const dirs = new Set<string>();
  for (const e of manifest.entries) {
    const parts = e.path.split('/');
    for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join('/'));
  }
  for (const dir of [...dirs].sort()) {
    await mkdir(join(dest, dir), { mode: 0o755 }).catch((err: NodeJS.ErrnoException) => {
      if (err.code !== 'EEXIST') throw err;
    });
  }
  const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
  for (const e of manifest.entries) {
    if (e.type !== 'file') continue;
    const handle = await open(
      join(dest, e.path),
      constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | noFollow,
      e.executable ? 0o755 : 0o644,
    );
    try {
      for await (const chunk of store.read(e.digest)) await handle.write(chunk as Buffer);
    } finally {
      await handle.close();
    }
  }
  for (const e of manifest.entries) {
    if (e.type === 'symlink') await symlink(e.target, join(dest, e.path));
  }
}

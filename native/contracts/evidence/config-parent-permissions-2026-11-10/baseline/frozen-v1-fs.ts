import { randomBytes } from 'node:crypto';
import { chmod, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';

export async function ensureDir(path: string, mode = 0o700): Promise<void> {
  await mkdir(path, { recursive: true, mode });
}

/** Writes a file atomically (temp file + rename) with the given permission bits. */
export async function writeFileAtomic(
  path: string,
  data: string | Uint8Array,
  mode = 0o600,
): Promise<void> {
  const dir = dirname(path);
  await ensureDir(dir);
  const tmp = join(dir, `.${randomBytes(6).toString('hex')}.tmp`);
  try {
    await writeFile(tmp, data, { mode });
    await chmod(tmp, mode);
    await rename(tmp, path);
  } catch (err) {
    await rm(tmp, { force: true });
    throw err;
  }
}

export async function readJsonFile<T = unknown>(path: string): Promise<T | undefined> {
  try {
    return JSON.parse(await readFile(path, 'utf8')) as T;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw err;
  }
}

export async function writeJsonFile(path: string, value: unknown, mode = 0o600): Promise<void> {
  await writeFileAtomic(path, JSON.stringify(value, null, 2) + '\n', mode);
}

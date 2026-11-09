import { readdir } from 'node:fs/promises';
import { join } from 'node:path';

const SKIP = new Set([
  'node_modules',
  '.git',
  'dist',
  'build',
  'coverage',
  '.next',
  '.turbo',
  'target',
  'vendor',
]);

/** Lists files (relative POSIX paths) up to `maxDepth` directories deep, skipping heavy dirs. */
export async function walkFiles(
  root: string,
  match: (name: string) => boolean,
  maxDepth = 4,
): Promise<string[]> {
  const out: string[] = [];
  const visit = async (rel: string, depth: number) => {
    let entries;
    try {
      entries = await readdir(rel === '' ? root : join(root, rel), { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const p = rel === '' ? e.name : `${rel}/${e.name}`;
      if (e.isDirectory()) {
        if (
          depth < maxDepth &&
          !SKIP.has(e.name) &&
          !(e.name.startsWith('.') && e.name !== '.github')
        )
          await visit(p, depth + 1);
      } else if (e.isFile() && match(e.name)) {
        out.push(p);
      }
    }
  };
  await visit('', 0);
  return out.sort();
}

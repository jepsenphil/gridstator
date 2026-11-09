import { readFile, access } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { walkFiles } from './walk.js';

export type PackageManager = 'pnpm' | 'npm' | 'yarn' | 'bun';

export interface PackageScripts {
  /** Directory of the package.json relative to the project root ("." for the root). */
  dir: string;
  name: string;
  packageManager: PackageManager;
  scripts: Record<string, string>;
}

async function exists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function detectPackageManager(
  root: string,
  dir: string,
  declared: unknown,
): Promise<PackageManager> {
  if (typeof declared === 'string') {
    const pm = declared.split('@')[0];
    if (pm === 'pnpm' || pm === 'yarn' || pm === 'npm' || pm === 'bun') return pm;
  }
  for (const d of dir === '.' ? ['.'] : [dir, '.']) {
    const base = join(root, d);
    if (await exists(join(base, 'pnpm-lock.yaml'))) return 'pnpm';
    if (await exists(join(base, 'yarn.lock'))) return 'yarn';
    if ((await exists(join(base, 'bun.lockb'))) || (await exists(join(base, 'bun.lock'))))
      return 'bun';
    if (await exists(join(base, 'package-lock.json'))) return 'npm';
  }
  return 'npm';
}

/** Finds package.json scripts in the project (root and nested packages, excluding node_modules). */
export async function discoverPackageScripts(root: string): Promise<PackageScripts[]> {
  const files = await walkFiles(root, (n) => n === 'package.json', 3);
  const rootPkg = await readFile(join(root, 'package.json'), 'utf8').then(
    (t) => JSON.parse(t) as Record<string, unknown>,
    () => undefined,
  );
  const result: PackageScripts[] = [];
  for (const file of files) {
    let pkg: Record<string, unknown>;
    try {
      pkg = JSON.parse(await readFile(join(root, file), 'utf8')) as Record<string, unknown>;
    } catch {
      continue;
    }
    const scripts =
      typeof pkg['scripts'] === 'object' && pkg['scripts'] !== null
        ? (pkg['scripts'] as Record<string, unknown>)
        : {};
    const clean = Object.fromEntries(
      Object.entries(scripts).filter((e): e is [string, string] => typeof e[1] === 'string'),
    );
    if (Object.keys(clean).length === 0) continue;
    const dir = dirname(file) === '.' ? '.' : dirname(file);
    result.push({
      dir,
      name: typeof pkg['name'] === 'string' ? pkg['name'] : dir,
      packageManager: await detectPackageManager(
        root,
        dir,
        pkg['packageManager'] ?? rootPkg?.['packageManager'],
      ),
      scripts: clean,
    });
  }
  return result;
}

/**
 * Shell command that installs dependencies (using the worker's persistent package cache) and runs
 * a script. Installs run at the project root so workspaces resolve.
 */
export function scriptCommand(pm: PackageManager, script: string, dir: string): string {
  const cd = dir === '.' ? '' : ` && cd ${JSON.stringify(dir)}`;
  switch (pm) {
    case 'pnpm':
      return `corepack enable && pnpm install --frozen-lockfile${cd} && pnpm run ${JSON.stringify(script)}`;
    case 'yarn':
      return `corepack enable && yarn install --immutable${cd} && yarn run ${JSON.stringify(script)}`;
    case 'bun':
      return `bun install --frozen-lockfile${cd} && bun run ${JSON.stringify(script)}`;
    case 'npm':
      return `npm ci${cd} && npm run ${JSON.stringify(script)}`;
  }
}

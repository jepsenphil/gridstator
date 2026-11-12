export type PackageManager = 'pnpm' | 'npm' | 'yarn' | 'bun';

export interface PackageScripts {
  /** Directory of the package.json relative to the project root ("." for the root). */
  dir: string;
  name: string;
  packageManager: PackageManager;
  scripts: Record<string, string>;
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

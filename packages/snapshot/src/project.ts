import { execFile } from 'node:child_process';
import { access, readFile } from 'node:fs/promises';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { promisify } from 'node:util';
import { parse as parseYaml } from 'yaml';
import { GridstatorError } from '@gridstator/shared';
import {
  PROJECT_CONFIG_FILENAMES,
  ProjectConfigSchema,
  type ProjectConfig,
} from '@gridstator/protocol';

const execFileAsync = promisify(execFile);

export interface ProjectLocation {
  /** Snapshot root (absolute). */
  root: string;
  /** Path of the caller's directory relative to the root, POSIX separators ("." for root). */
  workdir: string;
  configPath: string | undefined;
  /** Whether the root is inside a git work tree. */
  git: boolean;
}

async function exists(path: string): Promise<boolean> {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

async function findConfigUpwards(
  start: string,
  stopAt: string | undefined,
): Promise<string | undefined> {
  let dir = start;
  for (;;) {
    for (const name of PROJECT_CONFIG_FILENAMES) {
      const candidate = join(dir, name);
      if (await exists(candidate)) return candidate;
    }
    if (stopAt !== undefined && dir === stopAt) return undefined;
    const parent = dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

export async function gitTopLevel(dir: string): Promise<string | undefined> {
  try {
    const { stdout } = await execFileAsync('git', ['-C', dir, 'rev-parse', '--show-toplevel'], {
      encoding: 'utf8',
    });
    const top = stdout.trim();
    return top === '' ? undefined : resolve(top);
  } catch {
    return undefined;
  }
}

/**
 * Locates the project root for `cwd`: the nearest directory with a gridstator.yml (searching no
 * higher than the git top-level), otherwise the git top-level, otherwise `cwd` itself.
 */
export async function findProject(cwd: string): Promise<ProjectLocation> {
  const abs = resolve(cwd);
  const top = await gitTopLevel(abs);
  const configPath = await findConfigUpwards(abs, top);
  const root = configPath ? dirname(configPath) : (top ?? abs);
  const rel = relative(root, abs);
  return {
    root,
    workdir: rel === '' ? '.' : rel.split(sep).join('/'),
    configPath,
    git: top !== undefined,
  };
}

export async function loadProjectConfig(configPath: string | undefined): Promise<ProjectConfig> {
  if (!configPath) return ProjectConfigSchema.parse({});
  let raw: unknown;
  try {
    raw = parseYaml(await readFile(configPath, 'utf8')) ?? {};
  } catch (err) {
    throw new GridstatorError('CONFIG', `Cannot parse ${configPath}: ${(err as Error).message}`);
  }
  const parsed = ProjectConfigSchema.safeParse(raw);
  if (!parsed.success) {
    const issues = parsed.error.issues
      .map((i) => `  ${i.path.join('.') || '(root)'}: ${i.message}`)
      .join('\n');
    throw new GridstatorError('CONFIG', `Invalid ${basename(configPath)}:\n${issues}`);
  }
  return parsed.data;
}

export const DEFAULT_PROJECT_CONFIG_YAML = `# Gridstator project configuration. See https://github.com/gridstator/gridstator
version: 1

defaults:
  image: node:24-bookworm-slim
  resources:
    cpus: 2
    memory: 4g

snapshot:
  # Paths git ignores that should still be sent to workers.
  include: []
  # Additional paths to leave out (node_modules and .git are always excluded).
  exclude: []
  # Secret-looking files (e.g. .env.test) that are safe to send.
  allowSecrets: []

tasks:
  test:
    description: Install dependencies and run the test suite
    command: corepack enable && pnpm install --frozen-lockfile && pnpm test
`;

import { readFile } from 'node:fs/promises';
import { join, posix } from 'node:path';
import { walkFiles } from './walk.js';

export interface DockerfileInfo {
  /** Path relative to the project root. */
  file: string;
  /** Default build context: the Dockerfile's directory. */
  context: string;
  /** Named stages (`FROM … AS name`), usable as build targets. */
  stages: string[];
  baseImages: string[];
}

const isDockerfile = (name: string) =>
  /^Dockerfile(\..+)?$/.test(name) || /\.[Dd]ockerfile$/.test(name);

export function parseDockerfile(text: string): { stages: string[]; baseImages: string[] } {
  const stages: string[] = [];
  const baseImages: string[] = [];
  for (const raw of text.split('\n')) {
    const m = /^\s*FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?\s*$/i.exec(raw);
    if (!m) continue;
    baseImages.push(m[1]!);
    if (m[2]) stages.push(m[2]);
  }
  return { stages, baseImages };
}

export async function discoverDockerfiles(root: string): Promise<DockerfileInfo[]> {
  const files = await walkFiles(root, isDockerfile, 4);
  return Promise.all(
    files.map(async (file) => {
      const parsed = parseDockerfile(await readFile(join(root, file), 'utf8').catch(() => ''));
      const dir = posix.dirname(file);
      return { file, context: dir, ...parsed };
    }),
  );
}

// Sets one version across the repository: every package.json, the CLI/agent/controller version
// constant, and the VS Code extension. Used by the release workflow: node scripts/set-version.mjs 1.2.3
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const version = process.argv[2]?.replace(/^v/, '');
if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  process.stderr.write('usage: node scripts/set-version.mjs <semver>\n');
  process.exit(2);
}

const manifests = [join(root, 'package.json')];
for (const group of ['apps', 'packages']) {
  for (const name of await readdir(join(root, group)))
    manifests.push(join(root, group, name, 'package.json'));
}
for (const file of manifests) {
  let text;
  try {
    text = await readFile(file, 'utf8');
  } catch {
    continue;
  }
  const pkg = JSON.parse(text);
  // Only touch the version field so formatting stays exactly as it was.
  if (typeof pkg.version === 'string') {
    await writeFile(file, text.replace(/("version"\s*:\s*)"[^"]*"/, `$1"${version}"`));
  } else if (file === join(root, 'package.json')) {
    await writeFile(file, text.replace(/^\{\n/, `{\n  "version": "${version}",\n`));
  }
}

const constant = join(root, 'packages/shared/src/index.ts');
const src = await readFile(constant, 'utf8');
const next = src.replace(
  /export const GRIDSTATOR_VERSION = '[^']*';/,
  `export const GRIDSTATOR_VERSION = '${version}';`,
);
if (next === src && !src.includes(`'${version}'`))
  throw new Error('GRIDSTATOR_VERSION constant not found');
await writeFile(constant, next);
process.stdout.write(
  `Version set to ${version} in ${manifests.length} manifests and the version constant\n`,
);

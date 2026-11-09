// Prints the CHANGELOG.md section for a version (used as GitHub release notes).
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const version = (process.argv[2] ?? '').replace(/^v/, '');
const text = await readFile(
  join(dirname(fileURLToPath(import.meta.url)), '..', 'CHANGELOG.md'),
  'utf8',
);
const lines = text.split('\n');
const start = lines.findIndex(
  (l) => l.startsWith(`## [${version}]`) || l.startsWith(`## ${version}`),
);
if (start === -1) {
  process.stdout.write(`Gridstator ${version}\n`);
  process.exit(0);
}
const end = lines.findIndex((l, i) => i > start && l.startsWith('## '));
process.stdout.write(
  lines
    .slice(start + 1, end === -1 ? undefined : end)
    .join('\n')
    .trim() + '\n',
);

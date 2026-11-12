// Bundles the CLI (with controller, agent and all workspace packages) into a single ESM file and
// prepares a publishable `gridstator` npm package in apps/cli/release (see ADR-7).
import { cp, mkdir, readFile, rm, writeFile, chmod } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const here = dirname(fileURLToPath(import.meta.url));
const appDir = join(here, '..');
const repoRoot = join(appDir, '..', '..');
const out = join(appDir, 'release');
const pkg = JSON.parse(await readFile(join(appDir, 'package.json'), 'utf8'));

await rm(out, { recursive: true, force: true });
await mkdir(join(out, 'dist'), { recursive: true });

await build({
  entryPoints: [join(appDir, 'src', 'main.ts')],
  outfile: join(out, 'dist', 'gridstator.mjs'),
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node22',
  sourcemap: 'linked',
  legalComments: 'linked',
  // Optional native accelerators for `ws`; it works without them.
  external: ['bufferutil', 'utf-8-validate'],
  banner: {
    js: [
      "import { createRequire as __gstCreateRequire } from 'node:module';",
      "import { fileURLToPath as __gstFileURLToPath } from 'node:url';",
      "import { dirname as __gstDirname } from 'node:path';",
      'const require = __gstCreateRequire(import.meta.url);',
      'const __filename = __gstFileURLToPath(import.meta.url);',
      'const __dirname = __gstDirname(__filename);',
    ].join('\n'),
  },
  logLevel: 'warning',
});

const main = join(out, 'dist', 'gridstator.mjs');
const source = await readFile(main, 'utf8');
await writeFile(main, source.startsWith('#!') ? source : `#!/usr/bin/env node\n${source}`);
await chmod(main, 0o755);

await writeFile(
  join(out, 'package.json'),
  JSON.stringify(
    {
      name: 'gridstator',
      version: pkg.version,
      description: pkg.description,
      license: 'Apache-2.0',
      type: 'module',
      bin: { gridstator: './dist/gridstator.mjs' },
      files: ['dist', 'README.md', 'LICENSE'],
      engines: { node: '>=22.19.0' },
      keywords: [
        'distributed',
        'build',
        'test',
        'docker',
        'buildkit',
        'github-actions',
        'act',
        'cluster',
        'lan',
      ],
    },
    null,
    2,
  ) + '\n',
);
await cp(join(repoRoot, 'LICENSE'), join(out, 'LICENSE'));
await cp(join(repoRoot, 'docs/legacy/v1/README.md'), join(out, 'README.md'));
process.stdout.write(`Bundled ${main}\n`);

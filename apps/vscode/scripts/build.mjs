// Bundles the extension host code (CommonJS, `vscode` external) and the React dashboard webview.
import { copyFile, mkdir } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build, context } from 'esbuild';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');

// Host integration ships with the extension; it must not depend on an older global CLI.
const localCli = {
  entryPoints: [join(root, '../cli/src/main.ts')],
  outfile: join(root, 'dist/local-cli.mjs'),
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node22',
  external: ['bufferutil', 'utf-8-validate'],
  sourcemap: !production,
  minify: production,
  banner: {
    js: "import { createRequire as __gstCreateRequire } from 'node:module'; import { fileURLToPath as __gstFileURLToPath } from 'node:url'; import { dirname as __gstDirname } from 'node:path'; const require = __gstCreateRequire(import.meta.url); const __filename = __gstFileURLToPath(import.meta.url); const __dirname = __gstDirname(__filename);",
  },
  logLevel: 'warning',
};

/** @type {import('esbuild').BuildOptions} */
const extension = {
  entryPoints: [join(root, 'src/extension.ts')],
  outfile: join(root, 'dist/extension.js'),
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node20',
  external: ['vscode', 'bufferutil', 'utf-8-validate'],
  sourcemap: !production,
  minify: production,
  logLevel: 'warning',
};

/** @type {import('esbuild').BuildOptions} */
const webview = {
  entryPoints: [join(root, 'webview/main.tsx')],
  outfile: join(root, 'media/dashboard.js'),
  bundle: true,
  platform: 'browser',
  format: 'iife',
  target: 'es2022',
  jsx: 'automatic',
  sourcemap: !production,
  minify: production,
  define: { 'process.env.NODE_ENV': JSON.stringify(production ? 'production' : 'development') },
  logLevel: 'warning',
};

const codicons = dirname(require.resolve('@vscode/codicons/dist/codicon.css'));
await mkdir(join(root, 'media/codicons'), { recursive: true });
for (const f of ['codicon.css', 'codicon.ttf'])
  await copyFile(join(codicons, f), join(root, 'media/codicons', f));

if (watch) {
  const ctxs = await Promise.all([context(extension), context(webview), context(localCli)]);
  await Promise.all(ctxs.map((c) => c.watch()));
  process.stdout.write('Watching extension and webview sources…\n');
} else {
  await Promise.all([build(extension), build(webview), build(localCli)]);
  process.stdout.write(`Built extension and dashboard${production ? ' (production)' : ''}\n`);
}

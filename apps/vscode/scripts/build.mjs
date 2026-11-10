// Bundles the extension host code (CommonJS, `vscode` external) and the React dashboard webview.
import { copyFile, mkdir } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build, context } from 'esbuild';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');
const nativeOnly = process.argv.includes('--native');
const outputAt = process.argv.indexOf('--output-root');
if (outputAt >= 0 && !process.argv[outputAt + 1]) throw new Error('--output-root requires a path');
const outputRoot = outputAt < 0 ? root : resolve(process.argv[outputAt + 1]);
const nativeInstallTag =
  process.env.GRIDSTATOR_NATIVE_INSTALL_TAG ||
  (/^2\./.test(process.env.RELEASE_VERSION || '') ? `v${process.env.RELEASE_VERSION}` : '');
if (
  nativeInstallTag &&
  !/^v2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(nativeInstallTag)
)
  throw new Error('Native installation requires a valid v2 release tag');

// The v1 host helper stays bundled while native mode is an opt-in preview. Native mode resolves
// and probes a workspace-host executable through src/native.ts and sends private bridge requests.
const localCli = {
  entryPoints: [join(root, '../cli/src/main.ts')],
  outfile: join(outputRoot, 'dist/local-cli.mjs'),
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
  outfile: join(outputRoot, 'dist/extension.js'),
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node20',
  external: ['vscode', 'bufferutil', 'utf-8-validate'],
  sourcemap: !production,
  minify: production,
  define: {
    __GRIDSTATOR_NATIVE_ONLY__: JSON.stringify(nativeOnly),
    __GRIDSTATOR_NATIVE_INSTALL_TAG__: JSON.stringify(nativeInstallTag),
  },
  metafile: nativeOnly,
  plugins: nativeOnly
    ? [
        {
          name: 'native-workspace-boundary',
          setup(builder) {
            builder.onResolve({ filter: /^@gridstator\/(snapshot|discovery)$/ }, ({ path }) => ({
              path,
              namespace: 'native-boundary',
            }));
            builder.onLoad({ filter: /.*/, namespace: 'native-boundary' }, ({ path }) => ({
              resolveDir: root,
              contents: path.endsWith('/snapshot')
                ? 'const unavailable = () => { throw new Error("Native v2 workspace operations require the Rust CLI"); }; export { unavailable as captureSnapshot, unavailable as findProject, unavailable as loadProjectConfig, unavailable as uploadSnapshot };'
                : `export { scriptCommand } from ${JSON.stringify(join(root, '../../packages/discovery/src/scripts.ts'))}; const unavailable = () => { throw new Error("Native v2 discovery requires the Rust CLI"); }; export { unavailable as discoverDockerfiles, unavailable as discoverPackageScripts, unavailable as discoverWorkflows };`,
            }));
          },
        },
      ]
    : [],
  logLevel: 'warning',
};

/** @type {import('esbuild').BuildOptions} */
const webview = {
  entryPoints: [join(root, 'webview/main.tsx')],
  outfile: join(outputRoot, 'media/dashboard.js'),
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
await mkdir(join(outputRoot, 'dist'), { recursive: true });
await copyFile(
  join(root, '../../scripts/install-native.sh'),
  join(outputRoot, 'dist/install-native.sh'),
);
await mkdir(join(outputRoot, 'media/codicons'), { recursive: true });
for (const f of ['codicon.css', 'codicon.ttf'])
  await copyFile(join(codicons, f), join(outputRoot, 'media/codicons', f));

if (watch) {
  const ctxs = await Promise.all([
    context(extension),
    context(webview),
    ...(!nativeOnly ? [context(localCli)] : []),
  ]);
  await Promise.all(ctxs.map((c) => c.watch()));
  process.stdout.write('Watching extension and webview sources…\n');
} else {
  const [result] = await Promise.all([
    build(extension),
    build(webview),
    ...(!nativeOnly ? [build(localCli)] : []),
  ]);
  if (nativeOnly) {
    const inputs = Object.keys(result.metafile.inputs);
    if (inputs.some((path) => /packages\/snapshot\/|apps\/(cli|controller|agent)\//.test(path)))
      throw new Error('The native extension unexpectedly bundled legacy core implementation');
  }
  process.stdout.write(
    `Built ${nativeOnly ? 'native v2 ' : ''}extension and dashboard${production ? ' (production)' : ''}\n`,
  );
}

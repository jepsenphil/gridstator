import { build, context } from 'esbuild';
import { copyFile, mkdir, readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const production = process.argv.includes('--production');
const serve = process.argv.includes('--serve');
await mkdir(join(root, 'dist'), { recursive: true });
await copyFile(join(root, 'index.html'), join(root, 'dist/index.html'));
const options = {
  entryPoints: [join(root, 'src/main.tsx')],
  outfile: join(root, 'dist/app.js'),
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: ['safari15', 'chrome105', 'firefox105'],
  jsx: 'automatic',
  minify: production,
  sourcemap: !production,
  define: { 'process.env.NODE_ENV': JSON.stringify(production ? 'production' : 'development') },
  logLevel: 'warning',
};
if (serve) {
  const builder = await context(options);
  await builder.watch();
  // Only locally bundled files are served. This server is a development/build tool, never shipped.
  createServer(async (request, response) => {
    const filename = new URL(request.url, 'http://localhost').pathname.slice(1) || 'index.html';
    if (!/^(index\.html|app\.(?:js|css)(?:\.map)?)$/.test(filename)) {
      response.writeHead(404).end();
      return;
    }
    try {
      const bytes = await readFile(join(root, 'dist', filename));
      response.setHeader(
        'Content-Type',
        filename.endsWith('.html')
          ? 'text/html'
          : filename.endsWith('.css')
            ? 'text/css'
            : 'text/javascript',
      );
      response.setHeader('Cache-Control', 'no-store');
      response.end(bytes);
    } catch {
      response.writeHead(503).end('The desktop assets are building. Refresh in a moment.');
    }
  }).listen(1420, '127.0.0.1', () => process.stdout.write('Desktop UI: http://localhost:1420\n'));
} else {
  await build(options);
  process.stdout.write('Built Gridstator desktop assets\n');
}

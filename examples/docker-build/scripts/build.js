// A stand-in for a real build step (tsc, next build, vite build...): copies src to dist and
// stamps the version passed as a build argument.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';

mkdirSync('dist', { recursive: true });
const source = readFileSync('src/server.js', 'utf8');
writeFileSync('dist/server.js', source.replace('__VERSION__', process.env.APP_VERSION ?? 'dev'));
console.log('built dist/server.js');

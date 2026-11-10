// Build-host only: stamp Cargo's package and lockfile together before --locked builds.
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const version = process.argv[2];
if (!version || !/^2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error('usage: node native/scripts/set-version.mjs <2.x.y[-prerelease]>');
}
for (const [relative, pattern] of [
  ['../Cargo.toml', /(\[package\][\s\S]*?\nversion = )"[^"]+"/],
  ['../Cargo.lock', /(\[\[package\]\]\nname = "gridstator"\nversion = )"[^"]+"/],
]) {
  const path = fileURLToPath(new URL(relative, import.meta.url));
  const source = await readFile(path, 'utf8');
  if (!pattern.test(source)) throw new Error(`Could not locate native package in ${relative}`);
  await writeFile(path, source.replace(pattern, `$1"${version}"`));
}

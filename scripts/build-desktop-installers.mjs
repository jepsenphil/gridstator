// Keep CLI arguments intact, then collect this build's installer into the root distro folder.
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const app = join(root, 'apps/desktop');
const args = process.argv.slice(2);
const nativeOptions = new Set([
  '--native-release-dir',
  '--native-manifest-sha256',
  '--native-target',
]);
const nativeArguments = [];
for (let index = 0; index < args.length;) {
  const name = args[index].split('=')[0];
  if (!nativeOptions.has(name)) {
    index++;
    continue;
  }
  const equal = args[index].indexOf('=');
  const value = equal < 0 ? args[index + 1] : args[index].slice(equal + 1);
  if (!value || value.startsWith('--')) throw new Error(`${name} requires a value`);
  nativeArguments.push(name, value);
  args.splice(index, equal < 0 ? 2 : 1);
}
function run(command, values, capture = false) {
  const result = spawnSync(command, values, {
    cwd: join(app, 'src-tauri'),
    stdio: capture ? ['ignore', 'pipe', 'inherit'] : 'inherit',
    encoding: 'utf8',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status || 1);
  return result.stdout;
}
function option(...names) {
  for (let index = 0; index < args.length; index++) {
    if (names.includes(args[index])) return args[index + 1];
    for (const name of names) {
      if (args[index].startsWith(`${name}=`)) return args[index].slice(name.length + 1);
    }
  }
}

try {
  const explicitTarget = option('--target', '-t') || process.env.CARGO_BUILD_TARGET;
  const target = explicitTarget || run('rustc', ['-vV'], true).match(/^host: (.+)$/m)?.[1];
  const os = target?.includes('windows')
    ? 'windows'
    : target?.includes('apple-darwin')
      ? 'darwin'
      : target?.includes('linux')
        ? 'linux'
        : undefined;
  const architecture = target?.startsWith('aarch64-')
    ? 'arm64'
    : target?.startsWith('x86_64-')
      ? 'amd64'
      : undefined;
  if (!os || !architecture) throw new Error(`Unsupported desktop installer target: ${target}`);
  const nativeTarget = `${os === 'windows' ? 'linux' : os}/${architecture}`;
  const resources = join(app, 'src-tauri/resources/native-install.json');
  let staged = await readFile(resources, 'utf8')
    .then(JSON.parse)
    .catch(() => ({}));
  if (
    nativeArguments.length ||
    process.env.GRIDSTATOR_NATIVE_RELEASE_DIR ||
    (!staged.bundle && !staged.helper)
  ) {
    const stageArguments = [...nativeArguments];
    if (!stageArguments.includes('--native-target'))
      stageArguments.push('--native-target', nativeTarget);
    run(process.execPath, [
      join(root, 'scripts/desktop-artifacts.mjs'),
      'stage',
      ...stageArguments,
    ]);
    staged = JSON.parse(await readFile(resources, 'utf8'));
  }
  if (staged.bundle && !staged.bundle.targets.includes(nativeTarget))
    throw new Error(
      `Staged native bundle does not include ${nativeTarget}; stage the matching release first`,
    );
  if (staged.helper && staged.helper.target !== nativeTarget)
    throw new Error(`Staged native helper does not match ${nativeTarget}`);
  if (os === 'windows' && !staged.bundle && !staged.tag && !args.includes('--no-bundle'))
    throw new Error(
      'Windows installers require an offline native bundle. Pass --native-release-dir and --native-manifest-sha256, or explicitly pin a published native release with GRIDSTATOR_NATIVE_INSTALL_TAG.',
    );
  run(process.execPath, [join(app, 'node_modules/@tauri-apps/cli/tauri.js'), 'build', ...args]);
  if (args.includes('--no-bundle')) process.exit(0);
  const metadata = JSON.parse(
    run('cargo', ['metadata', '--no-deps', '--format-version', '1'], true),
  );
  const profile =
    option('--profile') || (args.includes('--debug') || args.includes('-d') ? 'debug' : 'release');
  const directory = join(metadata.target_directory, explicitTarget || '', profile, 'bundle');
  const config = JSON.parse(await readFile(join(app, 'src-tauri/tauri.conf.json'), 'utf8'));
  run(process.execPath, [
    join(root, 'scripts/desktop-artifacts.mjs'),
    'installers',
    '--bundle-dir',
    directory,
    '--target',
    `${os}/${architecture}`,
    '--version',
    config.version,
    '--build-id',
    process.env.GRIDSTATOR_DESKTOP_BUILD_ID || config.version,
    '--out',
    join(directory, 'installer-manifest.json'),
    '--distro-dir',
    join(root, 'distro'),
  ]);
} catch (error) {
  process.stderr.write(`Desktop installer build: ${error.message}\n`);
  process.exitCode = 1;
}

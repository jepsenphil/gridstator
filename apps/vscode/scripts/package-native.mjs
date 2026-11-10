// Build in a disposable staging directory: v1 outputs and source manifest remain usable.
import { execFile } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const require = createRequire(import.meta.url);
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const stage = await mkdtemp(join(tmpdir(), 'gridstator-native-vsix-'));
const output = resolve(process.argv[2] || join(root, 'dist/gridstator-native.vsix'));
try {
  const manifest = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  const cargo = await readFile(join(root, '../../native/Cargo.toml'), 'utf8');
  const nativeVersion = process.env.RELEASE_VERSION || cargo.match(/^version = "([^"]+)"/m)?.[1];
  if (!nativeVersion || !/^2\.\d+\.\d+(?:[-+][\w.-]+)?$/.test(nativeVersion))
    throw new Error('Native VSIX requires a v2 release version');
  // VS Code uses its prerelease channel rather than semver prerelease identifiers.
  manifest.version = nativeVersion.split(/[-+]/)[0];
  manifest.displayName = 'Gridstator (Native v2 Preview)';
  manifest.description = 'Set up or connect to Gridstator native v2 clusters from VS Code.';
  delete manifest.scripts;
  delete manifest.dependencies;
  delete manifest.devDependencies;
  manifest.files = [
    'dist/extension.js',
    'dist/install-native.sh',
    'media/**',
    'README.md',
    'LICENSE.md',
  ];
  delete manifest.contributes.configuration.properties['gridstator.nativeCli.enabled'];
  manifest.contributes.configuration.properties['gridstator.configPath'].markdownDescription =
    'Path to the shared native v2 CLI login on this workspace host. Empty uses gridstator-v2/client.json under the platform configuration directory.';
  await writeFile(join(stage, 'package.json'), JSON.stringify(manifest, null, 2) + '\n');
  await writeFile(
    join(stage, 'README.md'),
    '# Gridstator native v2 preview\n\nOpen Gridstator to set up a controller on the workspace host or connect to an existing one. Setup installs a verified native CLI for this account if it is missing, creates a background user service with a local worker, and signs in automatically. In SSH/WSL windows, setup runs on the workspace host. A local worker waits for Docker when it is unavailable.\n\nExisting CLI logins and controllers owned by this account are reused automatically. Explicit connection settings remain authoritative; existing native executables are never replaced during first-run setup. Manual installation remains available: set `gridstator.nativeCli.path` to the absolute native CLI path or put it on the host PATH. The CLI must support `bridge-v1` and `local-setup-v1` for first-run controller setup.\n\nThis preview delegates discovery, snapshots, submissions and local administration to Rust. It uses the separate `gridstator-v2` login by default. Create a fresh native cluster; v1 credentials and state remain separate. Docker, buildx and act/git remain worker tools. Node is supplied by VS Code for the editor UI and live API client.\n\nNative mode is required in this package. It includes no legacy JavaScript CLI helper. Full cross-platform release validation remains open.\n',
  );
  await copyFile(join(root, 'LICENSE.md'), join(stage, 'LICENSE.md'));
  const built = await execute(
    process.execPath,
    [join(root, 'scripts/build.mjs'), '--production', '--native', '--output-root', stage],
    {
      env: { ...process.env, GRIDSTATOR_NATIVE_INSTALL_TAG: `v${nativeVersion}` },
      maxBuffer: 8 * 1024 * 1024,
    },
  );
  process.stdout.write(built.stdout);
  process.stderr.write(built.stderr);
  for (const file of ['gridstator.svg', 'icon.png'])
    await copyFile(join(root, 'media', file), join(stage, 'media', file));
  await mkdir(dirname(output), { recursive: true });
  const packaged = await execute(
    process.execPath,
    [
      require.resolve('@vscode/vsce/vsce'),
      'package',
      '--pre-release',
      '--no-dependencies',
      '--out',
      output,
    ],
    { cwd: stage, maxBuffer: 8 * 1024 * 1024 },
  );
  process.stdout.write(packaged.stdout);
  process.stderr.write(packaged.stderr);
} finally {
  await rm(stage, { recursive: true, force: true });
}

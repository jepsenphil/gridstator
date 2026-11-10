// Build-host packaging only. The desktop application and native workers do not require Node.
import { createHash } from 'node:crypto';
import { copyFile, lstat, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const resourceRoot = join(root, 'apps/desktop/src-tauri/resources');
const supported = new Set([
  'linux/amd64',
  'linux/arm64',
  'darwin/amd64',
  'darwin/arm64',
  'windows/amd64',
  'windows/arm64',
]);
const coreTargets = ['linux/amd64', 'linux/arm64', 'darwin/amd64', 'darwin/arm64'];
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

function argumentsFor(values) {
  const options = new Map();
  for (let i = 0; i < values.length; i += 2) {
    if (!values[i]?.startsWith('--') || values[i + 1] === undefined || options.has(values[i]))
      throw new Error('Expected unique --name value arguments');
    options.set(values[i], values[i + 1]);
  }
  return options;
}

function required(options, name) {
  const value = options.get(name);
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

async function regularFile(path) {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink())
    throw new Error(`Expected a regular file: ${path}`);
  return readFile(path);
}

async function sourceDigest() {
  const files = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (
        ['node_modules', 'dist', 'target', 'gen', 'test-results', 'screenshots'].includes(
          entry.name,
        )
      )
        continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) files.push(path);
    }
  }
  for (const directory of ['apps/desktop', 'native/src', 'native/local-control-protocol']) {
    await visit(join(root, directory));
  }
  files.push(
    join(root, 'package.json'),
    join(root, 'pnpm-lock.yaml'),
    join(root, 'tsconfig.base.json'),
    join(root, 'scripts/desktop-artifacts.mjs'),
    join(root, 'scripts/build-desktop-installers.mjs'),
    join(root, 'scripts/install-native.sh'),
    join(root, 'native/Cargo.toml'),
    join(root, 'native/Cargo.lock'),
  );
  const hash = createHash('sha256');
  for (const path of files.sort()) {
    hash.update(relative(root, path));
    hash.update('\0');
    hash.update(await regularFile(path));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function nativeManifest(bytes, expected) {
  if (!/^[a-f0-9]{64}$/.test(expected || '') || digest(bytes) !== expected)
    throw new Error('Native manifest checksum mismatch');
  const manifest = JSON.parse(bytes);
  if (
    manifest.schemaVersion !== 1 ||
    !/^2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(manifest.version) ||
    typeof manifest.buildId !== 'string' ||
    !/^[A-Za-z0-9_.-]{1,128}$/.test(manifest.buildId) ||
    Object.keys(manifest.artifacts || {})
      .sort()
      .join(',') !== [...coreTargets].sort().join(',')
  )
    throw new Error('Expected the complete native four-target release manifest');
  return manifest;
}

async function stage(options) {
  const destination = options.has('--resource-dir')
    ? resolve(options.get('--resource-dir'))
    : resourceRoot;
  await mkdir(destination, { recursive: true });
  await copyFile(join(root, 'scripts/install-native.sh'), join(destination, 'install-native.sh'));
  const tag = options.get('--native-tag') || process.env.GRIDSTATOR_NATIVE_INSTALL_TAG || '';
  if (tag && !/^v2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(tag))
    throw new Error('Native release tag must be a complete v2 version');
  const configuration = {
    schemaVersion: 1,
    tag: tag || null,
    minimumCapabilities: ['bridge-v1', 'worker-control-v1', 'local-setup-v1'],
  };
  const binaryPath = options.get('--native-cli');
  const manifestPath = options.get('--native-manifest');
  const releasePath =
    options.get('--native-release-dir') || process.env.GRIDSTATOR_NATIVE_RELEASE_DIR;
  const expectedManifest =
    options.get('--native-manifest-sha256') || process.env.GRIDSTATOR_NATIVE_MANIFEST_SHA256;
  if (releasePath && (binaryPath || manifestPath))
    throw new Error('Choose a native release directory or an explicit native helper');
  if (Boolean(binaryPath) !== Boolean(manifestPath))
    throw new Error('Bundling a CLI requires both --native-cli and --native-manifest');
  if (binaryPath) {
    const target = required(options, '--native-target');
    if (!coreTargets.includes(target))
      throw new Error('The bundled helper must be a supported Linux/macOS core target');
    const bytes = await regularFile(resolve(binaryPath));
    const manifestBytes = await regularFile(resolve(manifestPath));
    const manifest = nativeManifest(manifestBytes, expectedManifest);
    const artifact = manifest.artifacts[target].executable;
    if (artifact.size !== bytes.length || artifact.sha256 !== digest(bytes))
      throw new Error('Bundled helper does not match its native manifest');
    const [os, arch] = target.split('/');
    if (os === 'linux') {
      if (
        bytes.length < 20 ||
        bytes.subarray(0, 4).toString('hex') !== '7f454c46' ||
        bytes[4] !== 2 ||
        bytes[5] !== 1 ||
        bytes.readUInt16LE(18) !== (arch === 'amd64' ? 62 : 183)
      )
        throw new Error('Bundled helper has the wrong executable target');
    } else if (
      bytes.length < 8 ||
      bytes.subarray(0, 4).toString('hex') !== 'cffaedfe' ||
      bytes.readUInt32LE(4) !== (arch === 'amd64' ? 0x01000007 : 0x0100000c)
    ) {
      throw new Error('Bundled helper has the wrong executable target');
    }
    await rm(join(destination, 'bin'), { recursive: true, force: true });
    await rm(join(destination, 'share'), { recursive: true, force: true });
    await mkdir(join(destination, 'bin'), { recursive: true });
    await copyFile(resolve(binaryPath), join(destination, 'bin/gridstator'));
    const releaseDirectory = join(destination, 'share/gridstator/releases', manifest.buildId);
    await mkdir(releaseDirectory, { recursive: true });
    await copyFile(resolve(manifestPath), join(releaseDirectory, 'gridstator-release.json'));
    configuration.helper = {
      target,
      buildId: manifest.buildId,
      sha256: artifact.sha256,
      manifestSha256: expectedManifest,
    };
  } else {
    // These are generated resources: do not carry an old optional helper into another package.
    await rm(join(destination, 'bin'), { recursive: true, force: true });
    await rm(join(destination, 'share'), { recursive: true, force: true });
  }
  if (releasePath) {
    const manifestBytes = await regularFile(join(resolve(releasePath), 'gridstator-release.json'));
    const manifest = nativeManifest(manifestBytes, expectedManifest);
    const targets = required(options, '--native-target').split(',');
    if (
      !targets.length ||
      new Set(targets).size !== targets.length ||
      targets.some((target) => !coreTargets.includes(target))
    )
      throw new Error('Choose supported native targets for the offline bundle');
    const archives = [];
    for (const target of targets) {
      const artifact = manifest.artifacts[target].archive;
      const expectedName = `gridstator-${manifest.version}-${target.replace('/', '-')}.tar.gz`;
      if (artifact.file !== expectedName) throw new Error('Unexpected native archive filename');
      const bytes = await regularFile(join(resolve(releasePath), artifact.file));
      if (
        !bytes.length ||
        bytes.length > 512 * 1024 * 1024 ||
        artifact.size !== bytes.length ||
        artifact.sha256 !== digest(bytes)
      )
        throw new Error('Native archive does not match its manifest');
      archives.push({ file: artifact.file, bytes });
    }
    const bundle = join(destination, 'native-bundle');
    await rm(bundle, { recursive: true, force: true });
    await mkdir(bundle, { recursive: true });
    await writeFile(join(bundle, 'gridstator-release.json'), manifestBytes);
    let checksums = `${expectedManifest}  gridstator-release.json\n`;
    for (const archive of archives) {
      await writeFile(join(bundle, archive.file), archive.bytes);
      checksums += `${digest(archive.bytes)}  ${archive.file}\n`;
    }
    await writeFile(join(bundle, 'SHA256SUMS'), checksums);
    configuration.bundle = {
      tag: `v${manifest.version}`,
      buildId: manifest.buildId,
      manifestSha256: expectedManifest,
      targets,
      archiveSha256: Object.fromEntries(
        targets.map((target) => [target, manifest.artifacts[target].archive.sha256]),
      ),
    };
  } else {
    await rm(join(destination, 'native-bundle'), { recursive: true, force: true });
  }
  await writeFile(
    join(destination, 'native-install.json'),
    JSON.stringify(configuration, null, 2) + '\n',
  );
  process.stdout.write(
    `Staged reviewed desktop resources${binaryPath ? ' and verified native helper' : releasePath ? ' and verified offline native bundle' : ''}\n`,
  );
}

async function receipt(options) {
  const target = required(options, '--target');
  if (!supported.has(target)) throw new Error('Unsupported desktop target');
  const path = resolve(required(options, '--artifact'));
  const bytes = await regularFile(path);
  const version = required(options, '--version');
  const buildId = required(options, '--build-id');
  if (!/^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/.test(version) || !/^[A-Za-z0-9_.-]{1,128}$/.test(buildId))
    throw new Error('Invalid desktop build identity');
  const result = {
    schemaVersion: 1,
    kind: 'gridstator-desktop',
    version,
    buildId,
    target,
    sourceState: 'working-tree',
    sourceSha256: await sourceDigest(),
    localControlProtocol: 1,
    requiredNativeCapabilities: ['bridge-v1', 'worker-control-v1', 'local-setup-v1'],
    artifact: { file: path.split(/[\\/]/).at(-1), size: bytes.length, sha256: digest(bytes) },
    signed: false,
    runtimeValidated: false,
  };
  await writeFile(resolve(required(options, '--out')), JSON.stringify(result, null, 2) + '\n');
  process.stdout.write(
    `Recorded ${target} desktop artifact; signing/runtime qualification remains explicit\n`,
  );
}

async function installers(options) {
  const target = required(options, '--target');
  if (!supported.has(target)) throw new Error('Unsupported desktop target');
  const version = required(options, '--version');
  const buildId = required(options, '--build-id');
  if (!/^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/.test(version) || !/^[A-Za-z0-9_.-]{1,128}$/.test(buildId))
    throw new Error('Invalid desktop build identity');
  const [os, architecture] = target.split('/');
  const format = { linux: 'deb', darwin: 'dmg', windows: 'nsis' }[os];
  const arch = {
    linux: { amd64: 'amd64', arm64: 'arm64' },
    darwin: { amd64: 'x64', arm64: 'aarch64' },
    windows: { amd64: 'x64', arm64: 'arm64' },
  }[os][architecture];
  const extension = { deb: '.deb', dmg: '.dmg', nsis: '-setup.exe' }[format];
  const directory = resolve(required(options, '--bundle-dir'));
  const filename = `Gridstator_${version}_${arch}${extension}`;
  const bytes = await regularFile(join(directory, format, filename)).catch(() => {
    throw new Error(`Missing ${target} installer: ${format}/${filename}`);
  });
  const valid = {
    deb: () => bytes.subarray(0, 8).toString() === '!<arch>\n',
    dmg: () => bytes.length >= 512 && bytes.subarray(-512, -508).toString() === 'koly',
    nsis: () => {
      if (bytes.length < 64 || bytes.subarray(0, 2).toString() !== 'MZ') return false;
      const peOffset = bytes.readUInt32LE(0x3c);
      return (
        peOffset <= bytes.length - 4 &&
        bytes.subarray(peOffset, peOffset + 4).equals(Buffer.from('PE\0\0')) &&
        bytes.includes(Buffer.from('efbeadde4e756c6c736f6674496e7374', 'hex'))
      );
    },
  }[format]();
  if (!valid) throw new Error(`Installer has an invalid ${format} container header`);
  const manifest = {
    schemaVersion: 1,
    kind: 'gridstator-desktop-installers',
    version,
    buildId,
    target,
    sourceState: 'working-tree',
    sourceSha256: await sourceDigest(),
    localControlProtocol: 1,
    requiredNativeCapabilities: ['bridge-v1', 'worker-control-v1', 'local-setup-v1'],
    installers: [
      { format, file: `${format}/${filename}`, size: bytes.length, sha256: digest(bytes) },
    ],
    signed: false,
    runtimeValidated: false,
  };
  await writeFile(resolve(required(options, '--out')), JSON.stringify(manifest, null, 2) + '\n');
  if (options.has('--distro-dir')) {
    await copyToDistro(manifest, directory, resolve(options.get('--distro-dir')));
  }
  process.stdout.write(
    `Recorded ${target} ${format} installer; signing/runtime qualification remains explicit\n`,
  );
}

async function copyToDistro(manifest, bundleDirectory, destination) {
  if (
    manifest.kind !== 'gridstator-desktop-installers' ||
    !supported.has(manifest.target) ||
    !/^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/.test(manifest.version) ||
    !Array.isArray(manifest.installers) ||
    !manifest.installers.length
  )
    throw new Error('Invalid installer manifest');
  const directory = join(destination, manifest.version);
  const installers = [];
  for (const installer of manifest.installers) {
    const filename = basename(installer.file);
    if (
      !['deb', 'dmg', 'nsis'].includes(installer.format) ||
      installer.file !== `${installer.format}/${filename}`
    )
      throw new Error('Invalid installer path in manifest');
    const bytes = await regularFile(join(bundleDirectory, installer.file));
    if (bytes.length !== installer.size || digest(bytes) !== installer.sha256)
      throw new Error(`Installer checksum mismatch: ${filename}`);
    await mkdir(directory, { recursive: true });
    await writeFile(join(directory, filename), bytes);
    installers.push({ ...installer, file: filename });
  }
  const filename = `installer-manifest-${manifest.target.replace('/', '-')}.json`;
  await writeFile(
    join(directory, filename),
    JSON.stringify({ ...manifest, installers }, null, 2) + '\n',
  );
  process.stdout.write(`Copied ${manifest.target} installers to ${directory}\n`);
}

try {
  const [command, ...values] = process.argv.slice(2);
  const options = argumentsFor(values);
  if (command === 'stage') await stage(options);
  else if (command === 'receipt') await receipt(options);
  else if (command === 'installers') await installers(options);
  else if (command === 'distro') {
    const path = resolve(required(options, '--manifest'));
    await copyToDistro(
      JSON.parse(await regularFile(path)),
      dirname(path),
      resolve(options.get('--distro-dir') || join(root, 'distro')),
    );
  } else
    throw new Error(
      'Usage: desktop-artifacts.mjs stage [--native-tag v2.x.y] | receipt --artifact path --target os/arch --version version --build-id id --out path | installers --bundle-dir path --target os/arch --version version --build-id id --out path [--distro-dir path] | distro --manifest path [--distro-dir path]',
    );
} catch (error) {
  process.stderr.write(`Desktop packaging: ${error.message}\n`);
  process.exitCode = 1;
}

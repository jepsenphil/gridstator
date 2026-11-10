import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterEach, beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { promisify } from 'node:util';

const execute = promisify(execFile);
const tool = resolve('scripts/desktop-artifacts.mjs');
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
let directory;
beforeEach(async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-desktop-packaging-'));
});
afterEach(async () => {
  await rm(directory, { recursive: true, force: true });
});

async function fixture() {
  // Header-only parsing fixture, deliberately not a platform runtime acceptance artifact.
  const binary = Buffer.alloc(64);
  binary.set([0x7f, 0x45, 0x4c, 0x46, 2, 1]);
  binary.writeUInt16LE(62, 18);
  const artifact = { file: 'gridstator', size: binary.length, sha256: hash(binary) };
  const manifest = {
    schemaVersion: 1,
    buildId: 'desktop-test-fixture',
    version: '2.0.0-dev.1',
    sourceRevision: 'test-fixture',
    protocol: { api: 1, agent: 1, update: 1 },
    artifacts: Object.fromEntries(
      ['linux/amd64', 'linux/arm64', 'darwin/amd64', 'darwin/arm64'].map((target) => [
        target,
        { executable: artifact, archive: { ...artifact, file: 'fixture.tar.gz' } },
      ]),
    ),
  };
  const binaryPath = join(directory, 'gridstator');
  const manifestPath = join(directory, 'native.json');
  const bytes = JSON.stringify(manifest);
  await writeFile(binaryPath, binary);
  await writeFile(manifestPath, bytes);
  return { binary, bytes, binaryPath, manifestPath };
}
function stageArgs(input, extra = []) {
  return [
    tool,
    'stage',
    '--resource-dir',
    join(directory, 'resources'),
    '--native-cli',
    input.binaryPath,
    '--native-manifest',
    input.manifestPath,
    '--native-target',
    'linux/amd64',
    '--native-manifest-sha256',
    hash(input.bytes),
    ...extra,
  ];
}

test('stages only a checksum-matched core helper with the complete native manifest', async () => {
  const input = await fixture();
  await execute(process.execPath, stageArgs(input));
  assert.deepEqual(await readFile(join(directory, 'resources/bin/gridstator')), input.binary);
  const config = JSON.parse(
    await readFile(join(directory, 'resources/native-install.json'), 'utf8'),
  );
  assert.equal(config.helper.sha256, hash(input.binary));
  assert.deepEqual(config.minimumCapabilities, [
    'bridge-v1',
    'worker-control-v1',
    'local-setup-v1',
  ]);
  assert.equal(
    await readFile(
      join(
        directory,
        'resources/share/gridstator/releases/desktop-test-fixture/gridstator-release.json',
      ),
      'utf8',
    ),
    input.bytes,
  );
});

test('rejects changed helper bytes, wrong targets and changed manifests before staging executable bytes', async () => {
  const input = await fixture();
  await writeFile(input.binaryPath, Buffer.alloc(64));
  await assert.rejects(execute(process.execPath, stageArgs(input)), /does not match/);
  await assert.rejects(readFile(join(directory, 'resources/bin/gridstator')));
  await writeFile(input.binaryPath, input.binary);
  const args = stageArgs(input);
  args[args.indexOf('linux/amd64')] = 'linux/arm64';
  await assert.rejects(execute(process.execPath, args), /wrong executable target/);
  await writeFile(input.manifestPath, input.bytes + ' ');
  await assert.rejects(execute(process.execPath, stageArgs(input)), /checksum mismatch/);
});

test('unbundled staging removes a previous generated helper and retains reviewed installer bytes', async () => {
  await execute(process.execPath, stageArgs(await fixture()));
  await execute(process.execPath, [tool, 'stage', '--resource-dir', join(directory, 'resources')]);
  await assert.rejects(readFile(join(directory, 'resources/bin/gridstator')));
  assert.deepEqual(
    await readFile(join(directory, 'resources/install-native.sh')),
    await readFile('scripts/install-native.sh'),
  );
  const config = JSON.parse(
    await readFile(join(directory, 'resources/native-install.json'), 'utf8'),
  );
  assert.equal(config.helper, undefined);
});

test('stages verified native archives for offline WSL setup and removes stale bundles', async () => {
  const input = await fixture();
  const manifest = JSON.parse(input.bytes);
  const archive = Buffer.from('archive fixture: transport integrity only');
  const filename = 'gridstator-2.0.0-dev.1-linux-amd64.tar.gz';
  manifest.artifacts['linux/amd64'].archive = {
    file: filename,
    size: archive.length,
    sha256: hash(archive),
  };
  const bytes = JSON.stringify(manifest);
  await writeFile(join(directory, 'gridstator-release.json'), bytes);
  await writeFile(join(directory, filename), archive);
  const args = [
    tool,
    'stage',
    '--resource-dir',
    join(directory, 'resources'),
    '--native-release-dir',
    directory,
    '--native-target',
    'linux/amd64',
    '--native-manifest-sha256',
    hash(bytes),
  ];
  await execute(process.execPath, args);
  const config = JSON.parse(
    await readFile(join(directory, 'resources/native-install.json'), 'utf8'),
  );
  assert.equal(config.bundle.manifestSha256, hash(bytes));
  assert.deepEqual(config.bundle.targets, ['linux/amd64']);
  assert.equal(config.bundle.tag, 'v2.0.0-dev.1');
  assert.equal(config.bundle.archiveSha256['linux/amd64'], hash(archive));
  assert.deepEqual(await readFile(join(directory, 'resources/native-bundle', filename)), archive);
  await writeFile(join(directory, filename), Buffer.from('damaged archive'));
  await assert.rejects(execute(process.execPath, args), /archive.*match/i);
  await execute(process.execPath, [tool, 'stage', '--resource-dir', join(directory, 'resources')]);
  await assert.rejects(readFile(join(directory, 'resources/native-bundle', filename)));
});

test('desktop artifact receipts do not assert signing or runtime validation', async () => {
  const path = join(directory, 'desktop.deb');
  await writeFile(path, 'unvalidated package fixture');
  const out = join(directory, 'receipt.json');
  await execute(process.execPath, [
    tool,
    'receipt',
    '--artifact',
    path,
    '--target',
    'linux/amd64',
    '--version',
    '2.0.0-dev.1',
    '--build-id',
    'desktop-fixture',
    '--out',
    out,
  ]);
  const receipt = JSON.parse(await readFile(out, 'utf8'));
  assert.equal(receipt.kind, 'gridstator-desktop');
  assert.equal(receipt.signed, false);
  assert.equal(receipt.runtimeValidated, false);
  assert.equal(receipt.artifact.sha256, hash('unvalidated package fixture'));
});

function installerArgs(target) {
  return [
    tool,
    'installers',
    '--bundle-dir',
    directory,
    '--target',
    target,
    '--version',
    '2.0.0-dev.1',
    '--build-id',
    'installer-fixture',
    '--out',
    join(directory, 'installer-manifest.json'),
  ];
}

test('installer manifests require the correct OS and architecture package, never a portable ZIP', async () => {
  await mkdir(join(directory, 'nsis'));
  await writeFile(
    join(directory, 'Gridstator_2.0.0-dev.1_windows_amd64_preview.zip'),
    'zip fixture',
  );
  await writeFile(join(directory, 'nsis/gridstator-desktop.exe'), 'raw executable fixture');
  await assert.rejects(
    execute(process.execPath, installerArgs('windows/amd64')),
    /Missing.*installer/,
  );
  // Container header fixture only: this is not evidence of a runnable Windows installer.
  const bytes = Buffer.alloc(128);
  bytes.write('MZ');
  bytes.writeUInt32LE(64, 0x3c);
  bytes.write('PE\0\0', 64);
  const path = join(directory, 'nsis/Gridstator_2.0.0-dev.1_x64-setup.exe');
  // A renamed ordinary PE executable must not pass as an NSIS setup program.
  await writeFile(path, bytes);
  await assert.rejects(
    execute(process.execPath, installerArgs('windows/amd64')),
    /invalid nsis container/,
  );
  bytes.set(Buffer.from('efbeadde4e756c6c736f6674496e7374', 'hex'), 80);
  await writeFile(path, bytes);
  await execute(process.execPath, installerArgs('windows/amd64'));
  const manifest = JSON.parse(await readFile(join(directory, 'installer-manifest.json'), 'utf8'));
  assert.equal(manifest.kind, 'gridstator-desktop-installers');
  assert.equal(manifest.installers[0].format, 'nsis');
  assert.equal(manifest.installers[0].sha256, hash(bytes));
  assert.equal(manifest.signed, false);
  assert.equal(manifest.runtimeValidated, false);
  await assert.rejects(
    execute(process.execPath, installerArgs('windows/arm64')),
    /Missing.*installer/,
  );
  await writeFile(path, 'MZ misleading extension');
  await assert.rejects(
    execute(process.execPath, installerArgs('windows/amd64')),
    /invalid nsis container/,
  );
});

test('installer manifests cover Debian packages and macOS disk images', async () => {
  await mkdir(join(directory, 'deb'));
  await mkdir(join(directory, 'dmg'));
  await writeFile(join(directory, 'deb/Gridstator_2.0.0-dev.1_arm64.deb'), '!<arch>\nfixture');
  await execute(process.execPath, installerArgs('linux/arm64'));
  let manifest = JSON.parse(await readFile(join(directory, 'installer-manifest.json'), 'utf8'));
  assert.equal(manifest.installers[0].format, 'deb');
  const diskImage = Buffer.alloc(1024);
  diskImage.write('koly', 512);
  await writeFile(join(directory, 'dmg/Gridstator_2.0.0-dev.1_aarch64.dmg'), diskImage);
  await execute(process.execPath, installerArgs('darwin/arm64'));
  manifest = JSON.parse(await readFile(join(directory, 'installer-manifest.json'), 'utf8'));
  assert.equal(manifest.installers[0].format, 'dmg');
  await writeFile(join(directory, 'dmg/Gridstator_2.0.0-dev.1_x64.dmg'), 'broken disk image');
  await assert.rejects(
    execute(process.execPath, installerArgs('darwin/amd64')),
    /invalid dmg container/,
  );
});

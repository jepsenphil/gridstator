import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { writeClientConfig } from '@gridstator/client';
import { Uri } from 'vscode';
import { canSetUpController, setUpController, type SetupResult } from './setup';
import { NativeCli } from './native';
import { installNativeCli } from './native-install';
import { testHost } from './test-vscode';

vi.mock('./native-install', () => ({ installNativeCli: vi.fn() }));

let directory: string;
beforeEach(async () => {
  testHost.reset();
  vi.clearAllMocks();
  directory = await mkdtemp(join(tmpdir(), 'gridstator-first-run-'));
});
afterEach(async () => {
  vi.unstubAllEnvs();
  await rm(directory, { recursive: true, force: true });
});

it('remembers a verified installation when the account home is the open workspace', async () => {
  for (const key of [
    'GRIDSTATOR_URL',
    'GRIDSTATOR_TOKEN',
    'GRIDSTATOR_CA_FILE',
    'GRIDSTATOR_CONFIG',
  ])
    vi.stubEnv(key, undefined);
  testHost.configure('nativeCli.enabled', true);
  const executable = join(directory, '.local/bin/gridstator');
  const result: SetupResult = {
    status: 'connected',
    host: {
      available: true,
      hostname: 'fixture',
      dataDir: directory,
      host: '127.0.0.1',
      port: 7300,
      url: 'https://127.0.0.1:7300',
      aliases: [],
      service: { installed: true, state: 'active', scope: 'user' },
    },
    workerWaitingForDocker: false,
  };
  vi.mocked(installNativeCli).mockImplementation(async () => {
    await mkdir(join(directory, '.local/bin'), { recursive: true });
    await writeFile(
      executable,
      `#!${process.execPath}
if (process.argv[2] === 'build-info') {
  process.stdout.write(JSON.stringify({ version: '2.0.0-dev.1', buildId: 'fixture', target: '${process.platform === 'darwin' ? 'darwin' : 'linux'}/amd64', capabilities: ['bridge-v1', 'local-setup-v1'] }));
} else {
  process.stdin.resume();
  process.stdin.on('end', () => process.stdout.write(JSON.stringify(${JSON.stringify(result)})));
}
`,
    );
    await chmod(executable, 0o700);
    return executable;
  });
  const options = {
    homeDirectory: () => directory,
    workspaceRoots: () => [directory],
    environment: () => ({ ...process.env, PATH: '' }),
  };
  const native = new NativeCli(options);
  const nextWindow = new NativeCli(options);
  try {
    expect(await setUpController({ extensionUri: Uri.file(directory) }, native)).toEqual(result);
    // A new window must resolve the installed CLI without downloading it again or trusting
    // other project-provided PATH entries inside this home-directory workspace.
    expect(
      await nextWindow.execute({ operation: 'setup', configPath: join(directory, 'client.json') }),
    ).toEqual(result);
    expect(installNativeCli).toHaveBeenCalledOnce();
  } finally {
    native.dispose();
    nextWindow.dispose();
  }
});

it('offers setup only when the default connection is absent', async () => {
  const path = join(directory, 'client.json');
  expect(await canSetUpController('', path, {}, 'linux')).toBe(true);
  await writeClientConfig(
    {
      version: 1,
      url: 'https://controller:7300',
      caPem: 'ca',
      caFingerprint: 'pin',
      token: 'credential',
    },
    path,
  );
  expect(await canSetUpController('', path, {}, 'linux')).toBe(false);
});

it('keeps custom paths, environment selections, and damaged saved connections authoritative', async () => {
  const path = join(directory, 'client.json');
  expect(await canSetUpController(path, path, {}, 'linux')).toBe(false);
  for (const key of [
    'GRIDSTATOR_URL',
    'GRIDSTATOR_TOKEN',
    'GRIDSTATOR_CA_FILE',
    'GRIDSTATOR_CONFIG',
  ]) {
    expect(await canSetUpController('', path, { [key]: 'explicit' }, 'linux')).toBe(false);
    expect(await canSetUpController('', path, { [key]: '' }, 'linux')).toBe(false);
  }
  await writeFile(path, '{bad-json');
  expect(await canSetUpController('', path, {}, 'linux')).toBe(false);
});

it('offers native setup on macOS and Linux/WSL, and leaves native Windows to remote workspaces', async () => {
  const path = join(directory, 'client.json');
  expect(await canSetUpController('', path, {}, 'darwin')).toBe(true);
  expect(await canSetUpController('', path, {}, 'win32')).toBe(false);
});

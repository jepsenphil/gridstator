import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  NativeCancelledError,
  NativeCli,
  effectiveClientConfigPath,
  nativeClientConfigPath,
  nativeEnabled,
} from './native';
import { testHost } from './test-vscode';
import {
  AgentBuildSchema,
  ControllerBuildSchema,
  ToolchainSettingsSchema,
  ToolPlatformSchema,
  ReportedToolPlatformSchema,
} from '@gridstator/protocol';

let directory: string;
const clients: NativeCli[] = [];

beforeEach(async () => {
  testHost.reset();
  directory = await mkdtemp(join(tmpdir(), 'gridstator-native-adapter-'));
});
afterEach(async () => {
  for (const client of clients.splice(0)) client.dispose();
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  await rm(directory, { recursive: true, force: true });
});

async function executable(
  options: {
    version?: string;
    target?: string;
    capabilities?: string[];
    fail?: boolean;
    invalidJson?: boolean;
    huge?: boolean;
  } = {},
): Promise<string> {
  const path = join(directory, 'native CLI with spaces.cjs');
  const info = {
    version: options.version ?? '2.0.0-dev.1',
    buildId: 'native-test-build',
    target:
      options.target ??
      `${process.platform === 'darwin' ? 'darwin' : 'linux'}/${process.arch === 'arm64' ? 'arm64' : 'amd64'}`,
    capabilities: options.capabilities ?? ['bridge-v1'],
  };
  const code = `#!${process.execPath}
const fs = require('node:fs');
if (process.argv[2] === 'build-info') {
  process.stdout.write(JSON.stringify(${JSON.stringify(info)}));
} else if (process.argv[2] === 'bridge') {
  let input = '';
  process.stdin.on('data', chunk => input += chunk);
  process.stdin.on('end', () => {
    const request = JSON.parse(input);
    fs.writeFileSync(process.env.NATIVE_TRACE, JSON.stringify({args: process.argv.slice(2), request, pid: process.pid}), {mode: 0o600});
    if (request.operation === 'capture') { setInterval(() => {}, 1000); return; }
    if (${options.fail === true}) { process.stderr.write('operation failed: ' + request.spec.secretEnv.TOKEN); process.exitCode = 4; return; }
    if (${options.invalidJson === true}) { process.stdout.write('invalid secret response'); return; }
    if (${options.huge === true}) { process.stdout.write('x'.repeat(9 * 1024 * 1024)); return; }
    process.stdout.write(JSON.stringify({accepted: true, operation: request.operation}));
  });
} else { process.exitCode = 3; }
`;
  await writeFile(path, code);
  await chmod(path, 0o755);
  return path;
}

function client(path: string, options: { trusted?: boolean; timeoutMs?: number } = {}): NativeCli {
  const native = new NativeCli({
    enabled: () => true,
    path: () => path,
    trusted: () => options.trusted ?? true,
    environment: () => ({ ...process.env, NATIVE_TRACE: join(directory, 'trace.json') }),
    timeoutMs: options.timeoutMs,
  });
  clients.push(native);
  return native;
}

async function trace(): Promise<{ args: string[]; request: Record<string, unknown>; pid: number }> {
  return JSON.parse(await readFile(join(directory, 'trace.json'), 'utf8')) as {
    args: string[];
    request: Record<string, unknown>;
    pid: number;
  };
}

describe('native workspace-host adapter', () => {
  it('probes setup capability without replacing an existing older CLI', async () => {
    const path = await executable();
    const install = vi.fn();
    await expect(client(path).prepareSetup(install)).rejects.toThrow(
      'does not support controller setup',
    );
    expect(install).not.toHaveBeenCalled();
  });

  it('never installs over an explicit missing path or from an untrusted workspace', async () => {
    const install = vi.fn();
    await expect(client(join(directory, 'missing')).prepareSetup(install)).rejects.toThrow(
      'not found',
    );
    const path = await executable({ capabilities: ['bridge-v1', 'local-setup-v1'] });
    await expect(client(path, { trusted: false }).prepareSetup(install)).rejects.toThrow(
      'Trust this workspace',
    );
    expect(install).not.toHaveBeenCalled();
  });

  it('installs a missing default helper and requires its setup capability', async () => {
    const path = await executable({ capabilities: ['bridge-v1', 'local-setup-v1'] });
    const native = new NativeCli({
      enabled: () => true,
      trusted: () => true,
      path: () => '',
      homeDirectory: () => directory,
      environment: () => ({ PATH: '' }),
    });
    clients.push(native);
    const install = vi.fn(async () => path);
    await native.prepareSetup(install);
    expect(install).toHaveBeenCalledOnce();
  });

  it('gates setup operations before sending a request to an older helper', async () => {
    const path = await executable();
    await expect(
      client(path).execute({ operation: 'setup', configPath: join(directory, 'client.json') }),
    ).rejects.toThrow('does not support controller setup');
    await expect(readFile(join(directory, 'trace.json'))).rejects.toThrow();
  });
  it('accepts Rust build/toolchain metadata while keeping legacy managed platforms scoped', () => {
    const build = AgentBuildSchema.parse({
      id: 'native-build',
      version: '2.0.0-dev.1',
      sha256: null,
      managed: true,
      lastRollback: null,
      runtime: 'rust',
      target: 'darwin/arm64',
      tools: { act: '0.2.97', managed: ['act'] },
    });
    expect(build.runtime).toBe('rust');
    expect(build.node).toBeUndefined();
    expect(
      ControllerBuildSchema.parse({
        id: 'native-build',
        version: '2.0.0-dev.1',
        sha256: null,
        updatable: true,
        reason: null,
        runtime: 'rust',
        target: 'linux/amd64',
      }).runtime,
    ).toBe('rust');
    expect(ToolchainSettingsSchema.parse({ act: null })).toEqual({ act: null, node: null });
    expect(ReportedToolPlatformSchema.parse('darwin/arm64')).toBe('darwin/arm64');
    expect(ToolPlatformSchema.safeParse('darwin/arm64').success).toBe(false);
  });
  it('keeps preview mode opt-in and uses the separate native configuration by default', () => {
    vi.stubEnv('GRIDSTATOR_CONFIG', '');
    expect(nativeEnabled()).toBe(false);
    testHost.configure('nativeCli.enabled', true);
    expect(nativeEnabled()).toBe(true);
    expect(effectiveClientConfigPath()).toBe(nativeClientConfigPath());
    expect(effectiveClientConfigPath()).toContain('gridstator-v2');
    testHost.configure('configPath', '/explicit/shared-login.json');
    expect(effectiveClientConfigPath()).toBe('/explicit/shared-login.json');
  });
  it('requires Rust delegation in the v2 package even when a retained v1 setting disables it', () => {
    vi.stubGlobal('__GRIDSTATOR_NATIVE_ONLY__', true);
    testHost.configure('nativeCli.enabled', false);
    vi.stubEnv('GRIDSTATOR_CONFIG', '');
    expect(nativeEnabled()).toBe(true);
    expect(effectiveClientConfigPath()).toBe(nativeClientConfigPath());
  });

  it('probes compatibility and sends secrets only through private stdin JSON', async () => {
    const native = client(await executable());
    const result = await native.execute<{ accepted: boolean }>({
      operation: 'submit',
      root: directory,
      spec: { kind: 'command', secretEnv: { TOKEN: 'private-token-value' } },
      configPath: join(directory, 'v2-client.json'),
    });
    expect(result.accepted).toBe(true);
    const received = await trace();
    expect(received.args).toEqual(['bridge']);
    expect(JSON.stringify(received.args)).not.toContain('private-token-value');
    expect(received.request).toMatchObject({
      version: 1,
      operation: 'submit',
      spec: { secretEnv: { TOKEN: 'private-token-value' } },
    });
  });

  it('refuses execution in an untrusted workspace before starting the executable', async () => {
    const native = client(await executable(), { trusted: false });
    await expect(native.execute({ operation: 'discover', root: directory })).rejects.toThrow(
      'Trust this workspace',
    );
    await expect(readFile(join(directory, 'trace.json'))).rejects.toMatchObject({ code: 'ENOENT' });
  });

  it('requires the native major version and bridge capability', async () => {
    const wrongVersion = client(await executable({ version: '0.1.0' }));
    await expect(wrongVersion.execute({ operation: 'discover' })).rejects.toThrow('bridge-v1');
    const missingCapability = client(await executable({ capabilities: [] }));
    await expect(missingCapability.execute({ operation: 'discover' })).rejects.toThrow('bridge-v1');
    await expect(readFile(join(directory, 'trace.json'))).rejects.toMatchObject({ code: 'ENOENT' });
  });

  it('accepts a compatible OS child independently of the editor process architecture', async () => {
    const target = `${process.platform === 'darwin' ? 'darwin' : 'linux'}/${process.arch === 'arm64' ? 'amd64' : 'arm64'}`;
    const native = client(await executable({ target }));
    await expect(native.execute({ operation: 'discover' })).resolves.toMatchObject({
      accepted: true,
    });
    const foreign = client(
      await executable({ target: process.platform === 'darwin' ? 'linux/amd64' : 'darwin/amd64' }),
    );
    await expect(foreign.execute({ operation: 'discover' })).rejects.toThrow('bridge-v1');
  });

  it('does not implicitly find a workspace-supplied executable through PATH', async () => {
    const source = await executable();
    const path = join(directory, 'gridstator');
    await writeFile(path, await readFile(source));
    await chmod(path, 0o755);
    const native = new NativeCli({
      enabled: () => true,
      path: () => '',
      trusted: () => true,
      workspaceRoots: () => [directory],
      environment: () => ({ PATH: directory }),
      homeDirectory: () => join(directory, 'account'),
    });
    clients.push(native);
    await expect(native.execute({ operation: 'discover' })).rejects.toThrow('not found');
  });
  it('finds the verified installer location when a graphical host has a minimal PATH', async () => {
    const home = join(directory, 'account');
    const bin = join(home, '.local', 'bin');
    await mkdir(bin, { recursive: true });
    const managed = join(bin, 'gridstator');
    await writeFile(managed, await readFile(await executable()));
    await chmod(managed, 0o755);
    const native = new NativeCli({
      enabled: () => true,
      path: () => '',
      trusted: () => true,
      workspaceRoots: () => [join(directory, 'project')],
      homeDirectory: () => home,
      environment: () => ({
        ...process.env,
        PATH: '',
        NATIVE_TRACE: join(directory, 'trace.json'),
      }),
    });
    clients.push(native);
    await expect(native.execute({ operation: 'discover' })).resolves.toMatchObject({
      accepted: true,
    });
  });

  it('reports missing executables with an installation action', async () => {
    const native = client(join(directory, 'missing'));
    await expect(native.execute({ operation: 'discover' })).rejects.toMatchObject({
      hint: expect.stringContaining('Install the native preview'),
    });
  });

  it('redacts request secrets from native diagnostic errors', async () => {
    const native = client(await executable({ fail: true }));
    const request = native.execute({
      operation: 'submit',
      spec: { secretEnv: { TOKEN: 'never-display-this' } },
    });
    await expect(request).rejects.toThrow('operation failed: [redacted]');
    await expect(request).rejects.not.toThrow('never-display-this');
  });

  it('cancels a running bridge child and reaps it', async () => {
    const native = client(await executable());
    const abort = new AbortController();
    const request = native.execute({ operation: 'capture', root: directory }, abort.signal);
    await expect.poll(async () => trace().catch(() => undefined)).not.toBeUndefined();
    const running = await trace();
    abort.abort();
    await expect(request).rejects.toBeInstanceOf(NativeCancelledError);
    expect(() => process.kill(running.pid, 0)).toThrow();
  });

  it('times out a hung bridge process and rejects malformed or excessive output', async () => {
    const hanging = client(await executable(), { timeoutMs: 80 });
    await expect(hanging.execute({ operation: 'capture' })).rejects.toThrow('timed out');
    const malformed = client(await executable({ invalidJson: true }));
    await expect(malformed.execute({ operation: 'discover' })).rejects.toThrow(
      'invalid bridge JSON',
    );
    const excessive = client(await executable({ huge: true }));
    await expect(excessive.execute({ operation: 'discover' })).rejects.toThrow('size limit');
  });

  it('reset aborts in-flight preview work instead of leaving it on the old connection', async () => {
    const native = client(await executable());
    const request = native.execute({ operation: 'capture' });
    await expect.poll(async () => trace().catch(() => undefined)).not.toBeUndefined();
    native.reset();
    await expect(request).rejects.toBeInstanceOf(NativeCancelledError);
  });
});

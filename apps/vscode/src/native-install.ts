import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { access } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import type { ReadableStreamDefaultReader } from 'node:stream/web';
import { GridstatorError } from '@gridstator/shared';

declare const __GRIDSTATOR_NATIVE_INSTALL_TAG__: string;

const RELEASES = 'https://api.github.com/repos/jepsenphil/gridstator/releases?per_page=100';
const TAG = /^v2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const installing = new Map<string, Promise<string>>();

interface InstallOptions {
  homeDirectory?: string;
  platform?: NodeJS.Platform;
  arch?: string;
  /** Native release VSIX builds pin their full version, including prerelease identifiers. */
  tag?: string;
  fetch?: typeof fetch;
  run?: typeof runInstaller;
}

export function nativeInstallTag(): string {
  return typeof __GRIDSTATOR_NATIVE_INSTALL_TAG__ === 'undefined'
    ? ''
    : __GRIDSTATOR_NATIVE_INSTALL_TAG__;
}

/** Runs the reviewed installer shipped in the VSIX. Only release data and archives are fetched. */
export async function installNativeCli(
  extensionDirectory: string,
  report: (message: string) => void = () => {},
  signal?: AbortSignal,
  options: InstallOptions = {},
): Promise<string> {
  signal?.throwIfAborted();
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  if (!['linux', 'darwin'].includes(platform) || !['x64', 'arm64'].includes(arch))
    throw new GridstatorError(
      'CONFIG',
      'Automatic setup requires Linux/WSL or macOS on x64 or ARM64',
    );
  const prefix = join(options.homeDirectory ?? homedir(), '.local');
  const executable = join(prefix, 'bin', 'gridstator');
  const active = installing.get(executable);
  if (active) return active;
  const task = (async () => {
    const selected = options.tag ?? nativeInstallTag();
    const target = `${platform}-${arch === 'x64' ? 'amd64' : 'arm64'}`;
    report('Finding a native Gridstator release…');
    const tag = selected || (await latestNativeTag(target, options.fetch ?? fetch, signal));
    if (!TAG.test(tag))
      throw new GridstatorError('CONFIG', 'The extension specifies an invalid native release tag');
    report(`Downloading and verifying Gridstator ${tag}…`);
    let contentions = 0;
    let deadline = 0;
    const lock = join(prefix, 'share/gridstator/.installer-lock');
    for (;;) {
      try {
        // The bundled script verifies any existing candidate against the selected official
        // release using its trusted downloaded verifier. Existence alone never grants trust.
        await (options.run ?? runInstaller)(
          join(extensionDirectory, 'dist', 'install-native.sh'),
          ['--tag', tag, '--prefix', prefix, '--if-missing'],
          signal,
        );
        break;
      } catch (error) {
        signal?.throwIfAborted();
        if (!(error instanceof Error) || !error.message.includes('another installation'))
          throw error;
        if (++contentions > 3) throw error;
        report('Waiting for another window to finish installing Gridstator…');
        deadline ||= Date.now() + 30_000;
        while ((await exists(lock)) || !(await exists(executable))) {
          if (Date.now() >= deadline)
            throw new GridstatorError(
              'UNAVAILABLE',
              'Another Gridstator installation has not finished',
              {
                hint: 'Wait for that installation to finish, then retry setup.',
              },
            );
          await delay(200, signal);
        }
        // Re-enter verification after the other installer releases its lock. Its executable may
        // belong to another release, so never return it merely because it appeared on disk.
      }
    }
    await access(executable, constants.X_OK).catch(() => {
      throw new GridstatorError('UNAVAILABLE', 'The installer did not produce an executable CLI', {
        hint: 'Retry setup, or install the native CLI manually and set gridstator.nativeCli.path.',
      });
    });
    report('Native Gridstator installed.');
    return executable;
  })();
  installing.set(executable, task);
  try {
    return await task;
  } finally {
    installing.delete(executable);
  }
}

async function exists(path: string): Promise<boolean> {
  try {
    await access(path, constants.F_OK);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
    throw error;
  }
}

async function latestNativeTag(target: string, fetchRelease: typeof fetch, signal?: AbortSignal) {
  try {
    const response = await fetchRelease(RELEASES, {
      headers: {
        accept: 'application/vnd.github+json',
        'user-agent': 'gridstator-vscode',
        'x-github-api-version': '2022-11-28',
      },
      signal: AbortSignal.any([AbortSignal.timeout(20_000), ...(signal ? [signal] : [])]),
      redirect: 'error',
    });
    if (!response.ok || !response.body) throw new Error(`GitHub returned HTTP ${response.status}`);
    const reader: ReadableStreamDefaultReader<unknown> = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let size = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!(value instanceof Uint8Array))
          throw new Error('GitHub returned invalid release bytes');
        size += value.length;
        if (size > 4 * 1024 * 1024) throw new Error('Release response exceeded its size limit');
        chunks.push(value);
      }
    } finally {
      await reader.cancel();
    }
    const raw: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (!Array.isArray(raw)) throw new Error('GitHub returned invalid release data');
    const releases = raw.filter(
      (
        value,
      ): value is {
        tag_name: string;
        draft: false;
        prerelease: boolean;
        published_at: string;
        assets: Array<{ name: string }>;
      } => {
        if (!value || typeof value !== 'object') return false;
        const release = value as Record<string, unknown>;
        if (
          typeof release.tag_name !== 'string' ||
          !TAG.test(release.tag_name) ||
          release.draft !== false ||
          typeof release.prerelease !== 'boolean' ||
          typeof release.published_at !== 'string' ||
          !Number.isFinite(Date.parse(release.published_at)) ||
          !Array.isArray(release.assets)
        )
          return false;
        const names = new Set(
          release.assets.map((asset: unknown) =>
            asset && typeof asset === 'object' && 'name' in asset ? asset.name : undefined,
          ),
        );
        return [
          'SHA256SUMS',
          'gridstator-release.json',
          `gridstator-${release.tag_name.slice(1)}-${target}.tar.gz`,
        ].every((name) => names.has(name));
      },
    );
    releases.sort(
      (a, b) =>
        Number(a.prerelease) - Number(b.prerelease) ||
        Date.parse(b.published_at) - Date.parse(a.published_at),
    );
    const selected = releases[0];
    if (!selected) throw new Error('No published native v2 release is available for this host');
    return selected.tag_name;
  } catch (error) {
    signal?.throwIfAborted();
    throw new GridstatorError('UNAVAILABLE', 'Could not find a native Gridstator download', {
      hint: 'Check this workspace host’s internet access and retry, or install the native CLI manually.',
      cause: error,
    });
  }
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal?.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(cancellation(signal));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', abort);
      resolve();
    }, ms);
    signal?.addEventListener('abort', abort, { once: true });
  });
}

function runInstaller(script: string, args: string[], signal?: AbortSignal): Promise<void> {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawn('/bin/sh', [script, ...args], {
      stdio: ['ignore', 'ignore', 'pipe'],
      shell: false,
      detached: true,
      windowsHide: true,
    });
    let diagnostic = '';
    let timedOut = false;
    let escalation: NodeJS.Timeout | undefined;
    const terminate = () => {
      if (!child.pid) return;
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch {
        /* Already exited. */
      }
      escalation = setTimeout(() => {
        try {
          process.kill(-child.pid!, 'SIGKILL');
        } catch {
          /* Already exited. */
        }
      }, 1000);
      escalation.unref();
    };
    signal?.addEventListener('abort', terminate, { once: true });
    const timer = setTimeout(() => {
      timedOut = true;
      terminate();
    }, 16 * 60_000);
    timer.unref();
    const cleanup = () => {
      clearTimeout(timer);
      if (escalation) clearTimeout(escalation);
      signal?.removeEventListener('abort', terminate);
    };
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk: string) => {
      diagnostic += chunk.slice(0, Math.max(0, 8192 - diagnostic.length));
    });
    child.once('error', () => {
      cleanup();
      reject(new GridstatorError('UNAVAILABLE', 'Could not run the bundled Gridstator installer'));
    });
    child.once('close', (code) => {
      if ((signal?.aborted || timedOut) && child.pid) {
        try {
          process.kill(-child.pid, 'SIGKILL');
        } catch {
          /* Owned children already exited. */
        }
      }
      cleanup();
      if (signal?.aborted) return reject(cancellation(signal));
      if (timedOut)
        return reject(
          new GridstatorError('UNAVAILABLE', 'The native Gridstator download timed out'),
        );
      if (code !== 0)
        return reject(
          new GridstatorError(
            'UNAVAILABLE',
            diagnostic.trim() || 'Could not install the native Gridstator CLI',
            {
              hint: 'Check this workspace host’s internet access and retry setup.',
            },
          ),
        );
      resolve();
    });
  });
}

function cancellation(signal?: AbortSignal): Error {
  return signal?.reason instanceof Error ? signal.reason : new Error('Native setup cancelled');
}

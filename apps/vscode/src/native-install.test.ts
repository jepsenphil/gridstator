import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { installNativeCli } from './native-install';

type InstallRun = (script: string, args: string[], signal?: AbortSignal) => Promise<void>;

let directory: string;
let executable: string;

beforeEach(async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-native-install-'));
  executable = join(directory, '.local/bin/gridstator');
});

afterEach(async () => {
  await rm(directory, { recursive: true, force: true });
});

async function publish(): Promise<void> {
  await mkdir(join(directory, '.local/bin'), { recursive: true });
  await writeFile(executable, 'installed native CLI', { mode: 0o700 });
}

function release(tag = 'v2.0.0-preview.1', prerelease = true, target = 'linux-amd64') {
  return {
    tag_name: tag,
    draft: false,
    prerelease,
    published_at: '2026-11-10T00:00:00Z',
    assets: [
      { name: 'SHA256SUMS' },
      { name: 'gridstator-release.json' },
      { name: `gridstator-${tag.slice(1)}-${target}.tar.gz` },
    ],
  };
}

describe('native CLI setup installation', () => {
  it('runs only the bundled installer with the full pinned native release tag', async () => {
    const run = vi.fn<InstallRun>(async () => publish());
    const fetchRelease = vi.fn<typeof fetch>();
    const report = vi.fn();
    expect(
      await installNativeCli('/extension with spaces', report, undefined, {
        homeDirectory: directory,
        platform: 'linux',
        arch: 'x64',
        tag: 'v2.0.0-preview.17+build.3',
        run,
        fetch: fetchRelease,
      }),
    ).toBe(executable);
    expect(fetchRelease).not.toHaveBeenCalled();
    expect(run).toHaveBeenCalledExactlyOnceWith(
      '/extension with spaces/dist/install-native.sh',
      ['--tag', 'v2.0.0-preview.17+build.3', '--prefix', join(directory, '.local'), '--if-missing'],
      undefined,
    );
    expect(report).toHaveBeenCalledWith('Native Gridstator installed.');
  });

  it('validates an existing default executable through the bundled verifier without replacing it', async () => {
    await publish();
    const run = vi.fn<InstallRun>(async () => {});
    const fetchRelease = vi.fn<typeof fetch>();
    expect(
      await installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: 'v2.0.0-preview.1',
        run,
        fetch: fetchRelease,
      }),
    ).toBe(executable);
    expect(run).toHaveBeenCalledExactlyOnceWith(
      join(directory, 'dist/install-native.sh'),
      ['--tag', 'v2.0.0-preview.1', '--prefix', join(directory, '.local'), '--if-missing'],
      undefined,
    );
    expect(fetchRelease).not.toHaveBeenCalled();
    expect(await readFile(executable, 'utf8')).toBe('installed native CLI');
  });

  it('propagates existing-candidate verification failure without trusting or replacing it', async () => {
    await publish();
    const run = vi
      .fn<InstallRun>()
      .mockRejectedValue(
        new Error('existing CLI does not match the selected verified release; left unchanged'),
      );
    await expect(
      installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: 'v2.0.0-preview.1',
        run,
      }),
    ).rejects.toThrow('does not match the selected verified release');
    expect(await readFile(executable, 'utf8')).toBe('installed native CLI');
    expect(run).toHaveBeenCalledOnce();
  });

  it('shares an installation across simultaneous windows in this host', async () => {
    const run = vi.fn(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
      await publish();
    });
    const options = { homeDirectory: directory, tag: 'v2.0.0-preview.1', run };
    const result = await Promise.all([
      installNativeCli(directory, undefined, undefined, options),
      installNativeCli(directory, undefined, undefined, options),
      installNativeCli(directory, undefined, undefined, options),
    ]);
    expect(result).toEqual([executable, executable, executable]);
    expect(run).toHaveBeenCalledOnce();
  });

  it('discovers only published v2 releases with complete artifacts for this host', async () => {
    const fetchRelease = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        new Response(
          JSON.stringify([
            release('v2.1.0-preview.1'),
            release('v2.0.0', false, 'darwin-arm64'),
            { ...release('v2.1.0', false), draft: true },
            { ...release('v2.2.0', false), assets: [] },
            release('v2.0.1', false),
            release('v1.0.0', false),
          ]),
        ),
      );
    const run = vi.fn<InstallRun>(async () => publish());
    await installNativeCli(directory, undefined, undefined, {
      homeDirectory: directory,
      platform: 'linux',
      arch: 'x64',
      tag: '',
      fetch: fetchRelease,
      run,
    });
    expect(fetchRelease.mock.calls[0]?.[0]).toBe(
      'https://api.github.com/repos/jepsenphil/gridstator/releases?per_page=100',
    );
    expect(run.mock.calls[0]?.[1]).toContain('v2.0.1');
  });

  it('falls back to a published preview when no stable native release exists', async () => {
    const run = vi.fn<InstallRun>(async () => publish());
    const fetchRelease = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify([
          {
            ...release('v2.0.0-preview.1', true, 'darwin-arm64'),
            published_at: '2026-11-09T00:00:00Z',
          },
          release('v2.0.0-preview.2', true, 'darwin-arm64'),
        ]),
      ),
    );
    await installNativeCli(directory, undefined, undefined, {
      homeDirectory: directory,
      platform: 'darwin',
      arch: 'arm64',
      tag: '',
      fetch: fetchRelease,
      run,
    });
    expect(run.mock.calls[0]?.[1]).toContain('v2.0.0-preview.2');
  });

  it('leaves offline failures retryable without invoking an installer', async () => {
    const run = vi.fn(async () => publish());
    const fetchRelease = vi.fn<typeof fetch>().mockRejectedValueOnce(new Error('offline'));
    const options = { homeDirectory: directory, tag: '', run, fetch: fetchRelease };
    await expect(installNativeCli(directory, undefined, undefined, options)).rejects.toMatchObject({
      message: 'Could not find a native Gridstator download',
      hint: expect.stringContaining('internet access'),
    });
    expect(run).not.toHaveBeenCalled();
    fetchRelease.mockResolvedValueOnce(new Response(JSON.stringify([release()])));
    expect(await installNativeCli(directory, undefined, undefined, options)).toBe(executable);
  });

  it('rejects oversized release listings and unsafe pinned tags before starting a child', async () => {
    const run = vi.fn();
    const fetchRelease = vi
      .fn<typeof fetch>()
      .mockResolvedValue(new Response(' '.repeat(4 * 1024 * 1024 + 1)));
    await expect(
      installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: '',
        run,
        fetch: fetchRelease,
      }),
    ).rejects.toThrow('Could not find a native Gridstator download');
    await expect(
      installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: 'v2.0.0; touch /tmp/unwanted',
        run,
      }),
    ).rejects.toThrow('invalid native release tag');
    expect(run).not.toHaveBeenCalled();
  });

  it('waits for another extension host to publish its atomic installation', async () => {
    const lock = join(directory, '.local/share/gridstator/.installer-lock');
    const run = vi
      .fn<InstallRun>()
      .mockImplementationOnce(async () => {
        await mkdir(lock, { recursive: true });
        await publish();
        setTimeout(() => {
          void rm(lock, { recursive: true });
        }, 20);
        throw new Error(
          'gridstator installer: another installation or an abandoned installer lock exists',
        );
      })
      .mockImplementationOnce(async () => {
        await expect(readFile(executable, 'utf8')).resolves.toBe('installed native CLI');
      });
    expect(
      await installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: 'v2.0.0-preview.1',
        run,
      }),
    ).toBe(executable);
    expect(run).toHaveBeenCalledTimes(2);
  });

  it('rejects a racing candidate when the second verifier run fails', async () => {
    const run = vi
      .fn<InstallRun>()
      .mockImplementationOnce(async () => {
        await publish();
        throw new Error('another installation owns the installer lock');
      })
      .mockRejectedValueOnce(
        new Error('existing CLI does not match the selected verified release; left unchanged'),
      );
    await expect(
      installNativeCli(directory, undefined, undefined, {
        homeDirectory: directory,
        tag: 'v2.0.0-preview.1',
        run,
      }),
    ).rejects.toThrow('does not match the selected verified release');
    expect(run).toHaveBeenCalledTimes(2);
    expect(await readFile(executable, 'utf8')).toBe('installed native CLI');
  });

  it('cancels the installer process group without installing anything', async () => {
    const dist = join(directory, 'dist');
    await mkdir(dist);
    const pids = join(directory, 'pids');
    await writeFile(
      join(dist, 'install-native.sh'),
      `sleep 60 &\nprintf '%s %s' "$$" "$!" > '${pids}'\nwait\n`,
    );
    await chmod(join(dist, 'install-native.sh'), 0o700);
    const signal = new AbortController();
    const result = installNativeCli(directory, undefined, signal.signal, {
      homeDirectory: directory,
      tag: 'v2.0.0-preview.1',
    });
    // Attach rejection handling before cancellation to avoid a transient unhandled rejection.
    const outcome = expect(result).rejects.toMatchObject({ name: 'AbortError' });
    await expect.poll(() => readFile(pids, 'utf8').catch(() => '')).not.toBe('');
    const children = (await readFile(pids, 'utf8')).split(' ').map(Number);
    signal.abort();
    await outcome;
    if (process.platform === 'linux') {
      await expect
        .poll(async () => {
          const states = await Promise.all(
            children.map((pid) => readFile(`/proc/${pid}/status`, 'utf8').catch(() => '')),
          );
          return states.every((state) => !state || /State:\s+Z/.test(state));
        })
        .toBe(true);
    }
  });
});

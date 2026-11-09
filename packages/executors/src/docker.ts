import { spawn, type ChildProcess } from 'node:child_process';
import type { Readable } from 'node:stream';
import type { ToolStatus } from '@gridstator/protocol';

export const LABEL_MANAGED = 'io.gridstator.managed';
export const LABEL_JOB = 'io.gridstator.job';
export const LABEL_NODE = 'io.gridstator.node';
export const LABEL_ASSIGNMENT = 'io.gridstator.assignment';
export const LABEL_CACHE = 'io.gridstator.cache';

export interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

export interface ExecOptions {
  input?: Readable;
  env?: NodeJS.ProcessEnv;
  timeoutMs?: number;
  onStderr?: (chunk: Buffer) => void;
}

export interface ManagedContainer {
  id: string;
  name: string;
  state: string;
  labels: Record<string, string>;
}

export interface ManagedVolume {
  name: string;
  labels: Record<string, string>;
}

function parseLabels(text: string): Record<string, string> {
  const labels: Record<string, string> = {};
  for (const pair of text.split(',')) {
    const i = pair.indexOf('=');
    if (i > 0) labels[pair.slice(0, i)] = pair.slice(i + 1);
  }
  return labels;
}

/**
 * Minimal wrapper over the `docker` CLI (ADR-13). Using the CLI means Docker contexts, DOCKER_HOST,
 * rootless Docker and Docker Desktop/WSL integration all behave exactly as they do for the user.
 */
export class DockerCli {
  readonly bin: string;

  constructor(bin = process.env['GRIDSTATOR_DOCKER'] ?? 'docker') {
    this.bin = bin;
  }

  spawn(args: string[], env?: NodeJS.ProcessEnv): ChildProcess {
    return spawn(this.bin, args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: env ?? process.env,
      windowsHide: true,
    });
  }

  exec(args: string[], options: ExecOptions = {}): Promise<ExecResult> {
    return new Promise((resolve, reject) => {
      const child = this.spawn(args, options.env);
      const out: Buffer[] = [];
      const err: Buffer[] = [];
      child.stdout?.on('data', (c: Buffer) => out.push(c));
      child.stderr?.on('data', (c: Buffer) => {
        err.push(c);
        options.onStderr?.(c);
      });
      let timer: NodeJS.Timeout | undefined;
      if (options.timeoutMs) timer = setTimeout(() => child.kill('SIGKILL'), options.timeoutMs);
      child.on('error', (e) => {
        if (timer) clearTimeout(timer);
        reject(e);
      });
      child.on('close', (code) => {
        if (timer) clearTimeout(timer);
        resolve({
          code: code ?? -1,
          stdout: Buffer.concat(out).toString('utf8'),
          stderr: Buffer.concat(err).toString('utf8'),
        });
      });
      if (options.input) {
        options.input.on('error', (e) => child.stdin?.destroy(e));
        child.stdin?.on('error', () => undefined);
        options.input.pipe(child.stdin!);
      } else {
        child.stdin?.end();
      }
    });
  }

  /** Runs a command and throws with Docker's stderr if it fails. */
  async check(args: string[], options: ExecOptions = {}): Promise<string> {
    const res = await this.exec(args, options);
    if (res.code !== 0) {
      throw new Error(`docker ${args[0]} failed: ${res.stderr.trim() || `exit code ${res.code}`}`);
    }
    return res.stdout;
  }

  async probe(): Promise<ToolStatus> {
    try {
      const res = await this.exec(['version', '--format', '{{.Server.Version}}'], {
        timeoutMs: 15_000,
      });
      if (res.code !== 0) {
        return {
          available: false,
          error: res.stderr.trim().split('\n').slice(-1)[0] || 'docker daemon unreachable',
        };
      }
      return { available: true, version: res.stdout.trim() };
    } catch (err) {
      return {
        available: false,
        error:
          (err as NodeJS.ErrnoException).code === 'ENOENT'
            ? 'docker CLI not installed'
            : String(err),
      };
    }
  }

  async probeBuildx(): Promise<ToolStatus> {
    try {
      const res = await this.exec(['buildx', 'version'], { timeoutMs: 15_000 });
      if (res.code !== 0)
        return { available: false, error: res.stderr.trim() || 'buildx not available' };
      const version = /v?(\d+\.\d+\.\d+)/.exec(res.stdout)?.[1];
      return { available: true, ...(version ? { version } : {}) };
    } catch {
      return { available: false, error: 'docker CLI not installed' };
    }
  }

  async ensureVolume(name: string, labels: Record<string, string>): Promise<void> {
    const args = ['volume', 'create'];
    for (const [k, v] of Object.entries(labels)) args.push('--label', `${k}=${v}`);
    await this.check([...args, name]);
  }

  async removeVolume(name: string): Promise<void> {
    await this.exec(['volume', 'rm', '-f', name]);
  }

  async copyInto(container: string, destDir: string, tar: Readable): Promise<void> {
    await this.check(['cp', '-', `${container}:${destDir}`], { input: tar });
  }

  async inspectState(
    container: string,
  ): Promise<{ exitCode: number; oomKilled: boolean; running: boolean; error: string }> {
    const out = await this.check(['inspect', '--format', '{{json .State}}', container]);
    const state = JSON.parse(out) as {
      ExitCode: number;
      OOMKilled: boolean;
      Running: boolean;
      Error: string;
    };
    return {
      exitCode: state.ExitCode,
      oomKilled: state.OOMKilled,
      running: state.Running,
      error: state.Error,
    };
  }

  async stop(container: string, graceSeconds: number): Promise<void> {
    await this.exec(['stop', '--time', String(graceSeconds), container]);
  }

  async removeContainer(container: string): Promise<void> {
    await this.exec(['rm', '--force', '--volumes', container]);
  }

  async listContainers(labelFilters: string[]): Promise<ManagedContainer[]> {
    const args = [
      'ps',
      '--all',
      '--no-trunc',
      '--format',
      '{{.ID}}\t{{.Names}}\t{{.State}}\t{{.Labels}}',
    ];
    for (const f of labelFilters) args.push('--filter', `label=${f}`);
    const out = await this.check(args);
    return out
      .split('\n')
      .filter((l) => l.trim() !== '')
      .map((line) => {
        const [id = '', name = '', state = '', labels = ''] = line.split('\t');
        return { id, name, state, labels: parseLabels(labels) };
      });
  }

  async listVolumes(labelFilters: string[]): Promise<ManagedVolume[]> {
    const args = ['volume', 'ls', '--format', '{{.Name}}\t{{.Labels}}'];
    for (const f of labelFilters) args.push('--filter', `label=${f}`);
    const out = await this.check(args);
    return out
      .split('\n')
      .filter((l) => l.trim() !== '')
      .map((line) => {
        const [name = '', labels = ''] = line.split('\t');
        return { name, labels: parseLabels(labels) };
      });
  }
}

export async function probeCommandVersion(bin: string, args: string[]): Promise<ToolStatus> {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    let out = '';
    child.stdout.on('data', (c: Buffer) => (out += c.toString()));
    child.stderr.on('data', (c: Buffer) => (out += c.toString()));
    child.on('error', () => resolve({ available: false, error: `${bin} not installed` }));
    child.on('close', (code) => {
      if (code !== 0)
        return resolve({ available: false, error: out.trim() || `exit code ${code}` });
      const version = /(\d+\.\d+\.\d+)/.exec(out)?.[1];
      resolve({ available: true, ...(version ? { version } : {}) });
    });
  });
}

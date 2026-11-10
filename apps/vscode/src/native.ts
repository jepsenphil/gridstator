import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { access, realpath, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { delimiter, isAbsolute, join, relative, resolve, sep } from 'node:path';
import * as vscode from 'vscode';
import { defaultClientConfigPath, GridstatorError } from '@gridstator/shared';
import { nativeExtensionBuild } from './build-flavor';

export interface NativeBuildInfo {
  buildId: string;
  version: string;
  target: string;
  capabilities: string[];
}

export interface NativeRequest {
  operation: 'discover' | 'capture' | 'submit' | 'local' | 'setup';
  root?: string;
  spec?: unknown;
  shards?: number;
  snapshotOptions?: unknown;
  args?: string[];
  configPath?: string;
  expectedClusterId?: string;
}

interface NativeOptions {
  enabled?: () => boolean;
  path?: () => string;
  trusted?: () => boolean;
  workspaceRoots?: () => readonly string[];
  environment?: () => NodeJS.ProcessEnv;
  homeDirectory?: () => string;
  timeoutMs?: number;
}

export class NativeCancelledError extends Error {
  constructor() {
    super('Native operation cancelled');
    this.name = 'NativeCancelledError';
  }
}

export function nativeEnabled(): boolean {
  return (
    nativeExtensionBuild() ||
    vscode.workspace.getConfiguration('gridstator').get<boolean>('nativeCli.enabled', false)
  );
}

export function nativeClientConfigPath(env: NodeJS.ProcessEnv = process.env): string {
  if (env.GRIDSTATOR_CONFIG) return env.GRIDSTATOR_CONFIG;
  const base =
    process.platform === 'darwin'
      ? join(homedir(), 'Library', 'Application Support')
      : env.XDG_CONFIG_HOME || join(homedir(), '.config');
  return join(base, 'gridstator-v2', 'client.json');
}

/** Shared by connection watchers, local administration and Settings so preview mode never falls
 * through to the v1 login merely because the user left configPath empty. */
export function effectiveClientConfigPath(): string {
  const path =
    vscode.workspace.getConfiguration('gridstator').get<string>('configPath', '') ||
    (nativeEnabled() ? nativeClientConfigPath() : defaultClientConfigPath());
  // Bridge capture runs from the project root. Preserve the extension host's interpretation of a
  // relative machine configuration path rather than resolving it differently in each project.
  return nativeEnabled() ? resolve(path) : path;
}

/** Runs the native executable on the workspace extension host. A shell is never involved and the
 * private bridge request, including secrets, is sent only through the child's stdin pipe. */
export class NativeCli implements vscode.Disposable {
  private readonly running = new Set<AbortController>();
  private generation = 0;
  private verified: { identity: string; info: NativeBuildInfo } | undefined;

  constructor(private readonly options: NativeOptions = {}) {}

  get enabled(): boolean {
    return this.options.enabled?.() ?? nativeEnabled();
  }

  reset(): void {
    this.generation += 1;
    this.verified = undefined;
    for (const request of this.running) request.abort();
  }

  dispose(): void {
    this.reset();
  }

  async execute<T>(request: NativeRequest, signal?: AbortSignal): Promise<T> {
    this.requireTrust();
    if (!this.enabled) throw new GridstatorError('CONFIG', 'Native CLI preview mode is disabled');
    if (signal?.aborted) throw new NativeCancelledError();
    const generation = this.generation;
    const executable = await this.resolveExecutable();
    const info = await this.probe(executable, signal);
    if (request.operation === 'setup') this.requireSetupCapability(info);
    if (generation !== this.generation) throw new NativeCancelledError();
    this.requireTrust();
    const payload = JSON.stringify({ ...request, version: 1 });
    const result = await this.run(
      executable,
      ['bridge'],
      payload,
      signal,
      this.options.timeoutMs ?? 10 * 60_000,
      request.root,
      secretValues(request),
    );
    try {
      return JSON.parse(result) as T;
    } catch {
      throw new GridstatorError('CONFIG', 'The native CLI returned invalid bridge JSON', {
        hint: 'Update the native CLI to a preview with bridge-v1 support.',
      });
    }
  }

  /** Install only a genuinely missing default CLI. Explicit paths and incompatible installs
   * remain authoritative, so onboarding never replaces an existing installation implicitly. */
  async prepareSetup(
    install: () => Promise<string>,
    signal?: AbortSignal,
  ): Promise<string | undefined> {
    this.requireTrust();
    let executable: string;
    let installed: string | undefined;
    try {
      executable = await this.resolveExecutable();
    } catch (err) {
      const explicit =
        this.options.path?.() ??
        vscode.workspace.getConfiguration('gridstator').get<string>('nativeCli.path', '');
      if (!(err instanceof NativeCliMissingError) || explicit) throw err;
      installed = await install();
      executable = installed;
    }
    this.requireTrust();
    this.requireSetupCapability(await this.probe(executable, signal));
    return installed;
  }

  private requireSetupCapability(info: NativeBuildInfo): void {
    if (!info.capabilities.includes('local-setup-v1'))
      throw new GridstatorError('CONFIG', 'This native CLI does not support controller setup', {
        hint: 'Install a matching Gridstator release with local-setup-v1 support, then retry setup.',
      });
  }

  private requireTrust(): void {
    if (!(this.options.trusted?.() ?? vscode.workspace.isTrusted)) {
      throw new GridstatorError('FORBIDDEN', 'Trust this workspace before running the native CLI');
    }
  }

  private async resolveExecutable(): Promise<string> {
    const explicit =
      this.options.path?.() ??
      vscode.workspace.getConfiguration('gridstator').get<string>('nativeCli.path', '');
    const environment = this.options.environment?.() ?? process.env;
    if (process.platform === 'win32') {
      throw new GridstatorError('CONFIG', 'Use the native CLI in a WSL or Remote SSH workspace');
    }
    if (explicit) {
      if (!isAbsolute(explicit))
        throw new GridstatorError(
          'CONFIG',
          'gridstator.nativeCli.path must be an absolute executable path',
        );
      return this.executableAt(explicit).catch(() => {
        throw missingCli(explicit);
      });
    }
    const roots =
      this.options.workspaceRoots?.() ??
      (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath);
    const directories = [
      join(this.options.homeDirectory?.() ?? homedir(), '.local', 'bin'),
      ...(environment.PATH ?? '').split(delimiter),
    ];
    for (const directory of new Set(directories)) {
      // Empty/relative PATH elements search the project directory; never execute them implicitly.
      if (!directory || !isAbsolute(directory)) continue;
      const candidate = join(directory, 'gridstator');
      let executable: string;
      try {
        executable = await this.executableAt(candidate);
      } catch {
        continue;
      }
      const canonicalRoots = await Promise.all(
        roots.map((root) => realpath(root).catch(() => resolve(root))),
      );
      if (canonicalRoots.some((root) => inside(root, candidate) || inside(root, executable)))
        continue;
      return executable;
    }
    throw missingCli();
  }

  private async executableAt(path: string): Promise<string> {
    await access(path, constants.X_OK);
    const executable = await realpath(path);
    if (!(await stat(executable)).isFile()) throw new Error('not a regular executable');
    return executable;
  }

  private async probe(executable: string, signal?: AbortSignal): Promise<NativeBuildInfo> {
    const metadata = await stat(executable);
    const identity = `${executable}:${metadata.dev}:${metadata.ino}:${metadata.size}:${metadata.mtimeMs}`;
    if (this.verified?.identity === identity) return this.verified.info;
    const output = await this.run(executable, ['build-info', '--json'], undefined, signal, 15_000);
    let info: NativeBuildInfo;
    try {
      info = JSON.parse(output) as NativeBuildInfo;
    } catch {
      throw incompatibleCli();
    }
    const platform = process.platform === 'darwin' ? 'darwin' : 'linux';
    // Rosetta can run an editor and a CLI of different architectures on the same Mac. The child
    // already executed successfully; require a supported OS target, not Node's process.arch.
    const targets = [`${platform}/amd64`, `${platform}/arm64`];
    if (
      !info ||
      typeof info.version !== 'string' ||
      !/^2\./.test(info.version) ||
      typeof info.buildId !== 'string' ||
      !info.buildId ||
      !targets.includes(info.target) ||
      !Array.isArray(info.capabilities) ||
      !info.capabilities.includes('bridge-v1')
    ) {
      throw incompatibleCli();
    }
    this.verified = { identity, info };
    return info;
  }

  private run(
    executable: string,
    args: string[],
    input: string | undefined,
    signal: AbortSignal | undefined,
    timeoutMs: number,
    cwd?: string,
    secrets: string[] = [],
  ): Promise<string> {
    if (signal?.aborted) return Promise.reject(new NativeCancelledError());
    const controller = new AbortController();
    this.running.add(controller);
    const cancel = () => controller.abort();
    signal?.addEventListener('abort', cancel, { once: true });
    return new Promise<string>((resolveResult, reject) => {
      let output = '';
      let diagnostic = '';
      let bytes = 0;
      let failure: Error | undefined;
      let escalation: NodeJS.Timeout | undefined;
      let terminating = false;
      const child = spawn(executable, args, {
        cwd,
        env: this.options.environment?.() ?? process.env,
        stdio: ['pipe', 'pipe', 'pipe'],
        windowsHide: true,
        detached: process.platform !== 'win32',
        shell: false,
      });
      const terminate = () => {
        if (terminating) return;
        terminating = true;
        if (child.pid) {
          try {
            if (process.platform !== 'win32') process.kill(-child.pid, 'SIGTERM');
            else child.kill('SIGTERM');
          } catch {
            /* It may already have exited. */
          }
        }
        escalation = setTimeout(() => {
          if (!child.pid) return;
          try {
            if (process.platform !== 'win32') process.kill(-child.pid, 'SIGKILL');
            else child.kill('SIGKILL');
          } catch {
            /* It may already have exited. */
          }
        }, 1500);
        escalation.unref();
      };
      const aborted = () => {
        failure = new NativeCancelledError();
        terminate();
      };
      controller.signal.addEventListener('abort', aborted, { once: true });
      const timer = setTimeout(() => {
        failure = new GridstatorError('UNAVAILABLE', 'The native CLI operation timed out');
        terminate();
      }, timeoutMs);
      timer.unref();
      const cleanup = () => {
        clearTimeout(timer);
        if (escalation) clearTimeout(escalation);
        signal?.removeEventListener('abort', cancel);
        controller.signal.removeEventListener('abort', aborted);
        this.running.delete(controller);
      };
      child.stdout.setEncoding('utf8');
      child.stderr.setEncoding('utf8');
      child.stdout.on('data', (chunk: string) => {
        bytes += Buffer.byteLength(chunk);
        if (bytes > 8 * 1024 * 1024) {
          failure = new GridstatorError(
            'UNAVAILABLE',
            'The native CLI response exceeded its size limit',
          );
          terminate();
        } else output += chunk;
      });
      child.stderr.on('data', (chunk: string) => {
        if (diagnostic.length < 64 * 1024)
          diagnostic += chunk.slice(0, 64 * 1024 - diagnostic.length);
      });
      child.once('error', () => {
        cleanup();
        reject(missingCli(executable));
      });
      child.once('close', (code) => {
        if (failure && child.pid && process.platform !== 'win32') {
          try {
            process.kill(-child.pid, 'SIGKILL');
          } catch {
            /* Owned descendants already exited. */
          }
        }
        cleanup();
        if (failure) {
          reject(failure);
          return;
        }
        if (code !== 0) {
          const safe = secrets.reduce(
            (text, secret) => text.split(secret).join('[redacted]'),
            diagnostic.trim(),
          );
          reject(
            new GridstatorError(
              'UNAVAILABLE',
              safe || `The native CLI exited with code ${code ?? 'unknown'}`,
            ),
          );
          return;
        }
        resolveResult(output);
      });
      child.stdin.on('error', () => {
        /* close/error reports the useful child failure. */
      });
      child.stdin.end(input);
    });
  }
}

function inside(root: string, path: string): boolean {
  const child = relative(resolve(root), resolve(path));
  return child === '' || (child !== '..' && !child.startsWith(`..${sep}`) && !isAbsolute(child));
}

function secretValues(request: unknown): string[] {
  const found = new Set<string>();
  const visit = (value: unknown, sensitive = false) => {
    if (typeof value === 'string') {
      if (sensitive && value) found.add(value);
      return;
    }
    if (!value || typeof value !== 'object') return;
    for (const [key, child] of Object.entries(value))
      visit(child, sensitive || /^(secretEnv|secrets|token|credential|password)$/i.test(key));
  };
  visit(request);
  return [...found].sort((a, b) => b.length - a.length);
}

class NativeCliMissingError extends GridstatorError {}

function missingCli(path?: string): GridstatorError {
  return new NativeCliMissingError(
    'CONFIG',
    `Native Gridstator CLI was not found${path ? ` at ${path}` : ' on the workspace host PATH'}`,
    {
      hint: 'Install the native preview on this workspace host, or set the machine setting gridstator.nativeCli.path to its absolute path.',
    },
  );
}
function incompatibleCli(): GridstatorError {
  return new GridstatorError(
    'CONFIG',
    'This native CLI does not support the extension bridge-v1 contract',
    {
      hint: 'Install a matching native v2 preview and check gridstator.nativeCli.path.',
    },
  );
}

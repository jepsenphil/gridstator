import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import * as vscode from 'vscode';
import { defaultClientConfigPath, errorMessage, GridstatorError } from '@gridstator/shared';
import { readClientConfig } from '@gridstator/client';
import { resolve } from 'node:path';
import type { LocalControllerHost } from './messages';
import { nativeClientConfigPath, type NativeCli } from './native';
import { nativeExtensionBuild } from './build-flavor';

const execFileAsync = promisify(execFile);

interface LoginResult {
  status: 'connected' | 'revoked' | 'unavailable' | 'differentCluster';
  message?: string;
}

/** Runs host administration locally, using the same OS account as the SSH extension host. */
export class LocalController {
  host: LocalControllerHost | null = null;
  canRestore = false;
  matchesLocal = false;
  private prepared = false;
  private preparing: Promise<void> | undefined;

  get canStart(): boolean {
    return Boolean(
      this.matchesLocal &&
      this.host?.available &&
      this.host.service.canManage &&
      ['inactive', 'failed'].includes(this.host.service.state),
    );
  }

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly configPath: () => string | undefined,
    private readonly log: vscode.OutputChannel,
    private readonly native?: NativeCli,
  ) {}

  private async run<T>(args: string[]): Promise<T> {
    if (this.native?.enabled) {
      return this.native.execute<T>({
        operation: 'local',
        args,
        configPath: this.configPath() || nativeClientConfigPath(),
      });
    }
    if (nativeExtensionBuild())
      throw new GridstatorError('CONFIG', 'Native v2 host administration requires the Rust CLI');
    const helper = vscode.Uri.joinPath(this.context.extensionUri, 'dist/local-cli.mjs').fsPath;
    try {
      const { stdout } = await execFileAsync(
        process.execPath,
        [helper, 'controller', 'local', ...args, '--json'],
        {
          env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
          timeout: 120_000,
          maxBuffer: 1024 * 1024,
          windowsHide: true,
        },
      );
      return JSON.parse(stdout) as T;
    } catch (err) {
      const e = err as Error & { stderr?: string };
      // Only local commands run here; their contract never prints credential values.
      throw new GridstatorError('CONFIG', e.stderr?.trim() || e.message);
    }
  }

  async refresh(): Promise<LocalControllerHost | null> {
    if (process.platform !== 'linux' && !(this.native?.enabled && process.platform === 'darwin'))
      return null;
    try {
      this.host = await this.run<LocalControllerHost>([
        'status',
        '--config',
        this.configPath() || this.defaultConfigPath(),
      ]);
    } catch (err) {
      this.log.appendLine(`Local controller: ${errorMessage(err)}`);
      this.host = null;
    }
    return this.host;
  }

  reset(): void {
    this.prepared = false;
    this.host = null;
    this.canRestore = false;
    this.matchesLocal = false;
  }

  private defaultConfigPath(): string {
    return this.native?.enabled ? nativeClientConfigPath() : defaultClientConfigPath();
  }

  async prepare(): Promise<void> {
    if (this.prepared) return;
    if (this.preparing) return this.preparing;
    this.preparing = this.prepareOnce().finally(() => {
      this.preparing = undefined;
    });
    return this.preparing;
  }

  private async prepareOnce(): Promise<void> {
    this.prepared = true;
    this.matchesLocal = false;
    await this.refresh();
    if (!this.host?.available) return;
    // Explicit environment connections remain authoritative.
    if (['GRIDSTATOR_URL', 'GRIDSTATOR_TOKEN', 'GRIDSTATOR_CA_FILE'].some((k) => process.env[k]))
      return;
    const configured = this.configPath();
    if (configured && resolve(configured) !== resolve(this.defaultConfigPath())) {
      const saved = await readClientConfig(configured).catch(() => undefined);
      if (!saved || saved.caFingerprint !== this.host.caFingerprint) return;
    }
    const result = await this.run<LoginResult>([
      'ensure-login',
      '--config',
      this.configPath() || this.defaultConfigPath(),
    ]);
    this.matchesLocal = result.status === 'connected' || result.status === 'revoked';
    this.canRestore = result.status === 'revoked';
    if (result.status === 'differentCluster')
      this.host = {
        ...this.host,
        available: false,
        reason: result.message || 'The saved connection selects another cluster.',
      };
    if (result.message) this.log.appendLine(`Local sign-in: ${result.message}`);
  }

  async restore(): Promise<void> {
    const result = await this.run<LoginResult>([
      'ensure-login',
      '--config',
      this.configPath() || this.defaultConfigPath(),
      '--restore',
    ]);
    if (result.status !== 'connected')
      throw new GridstatorError('CONFIG', result.message || 'Local access could not be restored');
    this.canRestore = false;
    this.matchesLocal = true;
    await this.refresh();
  }

  async service(action: 'start' | 'stop' | 'restart'): Promise<void> {
    await this.run(['service', action, '--config', this.configPath() || this.defaultConfigPath()]);
    await this.refresh();
  }

  async configure(host: string, port: number, url: string): Promise<void> {
    await this.run([
      'configure',
      '--host',
      host,
      '--port',
      String(port),
      '--url',
      url,
      '--config',
      this.configPath() || this.defaultConfigPath(),
    ]);
    await this.refresh();
  }
}

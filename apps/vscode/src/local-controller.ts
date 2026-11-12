import type * as vscode from 'vscode';
import { errorMessage, GridstatorError } from '@gridstator/shared';
import { readClientConfig } from '@gridstator/client';
import { resolve } from 'node:path';
import type { LocalControllerHost } from './messages';
import { nativeClientConfigPath, type NativeCli } from './native';

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
    private readonly configPath: () => string | undefined,
    private readonly log: vscode.OutputChannel,
    private readonly native: NativeCli,
  ) {}

  private async run<T>(args: string[]): Promise<T> {
    return this.native.execute<T>({
      operation: 'local',
      args,
      configPath: this.configPath() || nativeClientConfigPath(),
    });
  }

  async refresh(): Promise<LocalControllerHost | null> {
    if (process.platform !== 'linux' && process.platform !== 'darwin') return null;
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
    return nativeClientConfigPath();
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

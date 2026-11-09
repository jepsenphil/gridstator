import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { pipeline } from 'node:stream/promises';
import { resolve } from 'node:path';
import * as vscode from 'vscode';
import {
  GRIDSTATOR_VERSION,
  GridstatorError,
  defaultClientConfigPath,
  errorMessage,
} from '@gridstator/shared';
import { GridstatorClient, readClientConfig, resolveConnection } from '@gridstator/client';
import type { PublishBuildResponse } from '@gridstator/protocol';
import type { ClusterState } from './state';
import type { LocalController } from './local-controller';
import type { EditorPreferences, FromWebview, SettingsView, ToWebview } from './messages';

function preferences(): EditorPreferences {
  const c = vscode.workspace.getConfiguration('gridstator');
  return {
    autoConnect: c.get('autoConnect', true),
    configPath: c.get('configPath', ''),
    defaultNode: c.get('defaultNode', ''),
    unsavedFiles: c.get('unsavedFiles', 'prompt'),
    notifications: c.get('notifications', 'failures'),
    problemMatching: c.get('problemMatching', true),
    testResults: c.get('testResults', true),
    jobHistory: c.get('jobHistory', 100),
  };
}

interface PendingRestore {
  clusterId: string;
  nodeId: string;
  owner?: string;
  ready?: boolean;
  expiresAt?: number;
}
const RESTORE_KEY = 'controllerUpdate.pendingWorkerRestore';

/** Administrative data is loaded independently from the job stream; credentials never enter it. */
export class DashboardSettings implements vscode.Disposable {
  private view: SettingsView;
  private generation = 0;
  private loading: Promise<void> | undefined;
  private controllerInstall: AbortController | undefined;
  private restoring: Promise<void> | undefined;
  private readonly onConnection: vscode.Disposable;
  private readonly session = randomUUID();

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly state: ClusterState,
    private readonly local: LocalController,
    private readonly post: (message: ToWebview) => void,
    private readonly changed: () => void,
  ) {
    const metadata: unknown = context.extension.packageJSON;
    const version =
      metadata &&
      typeof metadata === 'object' &&
      'version' in metadata &&
      typeof metadata.version === 'string'
        ? metadata.version
        : GRIDSTATOR_VERSION;
    this.view = {
      preferences: preferences(),
      cluster: null,
      updates: null,
      tokens: null,
      releases: [],
      includePrereleases: false,
      host: null,
      extension: { version, build: 'unknown' },
    };
    this.onConnection = state.onDidChange(() => {
      if (state.connected && this.context.globalState.get<PendingRestore[]>(RESTORE_KEY, []).length)
        void this.restorePending();
    });
    void readFile(vscode.Uri.joinPath(context.extensionUri, 'dist/extension.js').fsPath)
      .then((bytes) => {
        this.view.extension.build = createHash('sha256').update(bytes).digest('hex').slice(0, 12);
        this.changed();
      })
      .catch(() => undefined);
  }

  snapshot(): SettingsView {
    return { ...this.view, preferences: preferences() };
  }

  refresh(): Promise<void> {
    if (this.loading) return this.loading;
    this.loading = this.load().finally(() => {
      this.loading = undefined;
    });
    return this.loading;
  }

  private async load(): Promise<void> {
    await this.restorePending();
    const gen = ++this.generation;
    const client = this.state.connected ? this.state.client : undefined;
    const clusterId = this.state.connection.clusterId;
    const host = await this.local.refresh();
    if (host?.available) {
      const path = preferences().configPath || defaultClientConfigPath();
      const saved = await readClientConfig(path).catch(() => undefined);
      if (
        (host.clusterId && clusterId && host.clusterId !== clusterId) ||
        (resolve(path) !== resolve(defaultClientConfigPath()) && !saved) ||
        (host.caFingerprint &&
          saved?.caFingerprint &&
          host.caFingerprint !== saved.caFingerprint) ||
        ['GRIDSTATOR_URL', 'GRIDSTATOR_TOKEN', 'GRIDSTATOR_CA_FILE'].some((k) => process.env[k])
      ) {
        this.view.host = {
          ...host,
          available: false,
          reason:
            'Host controls belong to the local controller. The current connection selects another cluster or uses environment overrides.',
        };
      } else this.view.host = host;
    } else this.view.host = host;
    if (this.view.host?.available && this.local.matchesLocal)
      this.state.connection.publicUrl = this.view.host.url;
    if (!client) {
      this.view = { ...this.view, cluster: null, updates: null, tokens: null };
      this.changed();
      return;
    }
    const [cluster, updates, tokens] = await Promise.allSettled([
      client.getSettings(),
      client.updates(),
      this.state.connection.role === 'admin' ? client.listTokens() : Promise.resolve(null),
    ]);
    if (
      gen !== this.generation ||
      client !== this.state.client ||
      clusterId !== this.state.connection.clusterId
    )
      return;
    this.view = {
      ...this.view,
      cluster: cluster.status === 'fulfilled' ? cluster.value : null,
      updates: updates.status === 'fulfilled' ? updates.value : null,
      tokens: tokens.status === 'fulfilled' ? tokens.value : null,
      error:
        [cluster, updates, tokens]
          .filter((r) => r.status === 'rejected')
          .map((r) => (r.status === 'rejected' ? errorMessage(r.reason) : ''))
          .join('; ') || null,
    };
    this.changed();
  }

  private requireAdmin(): void {
    if (this.state.connection.role !== 'admin')
      throw new GridstatorError('FORBIDDEN', 'This action requires an admin login');
  }

  private async requireHost(): Promise<void> {
    await this.refresh();
    if (!this.view.host?.available)
      throw new GridstatorError(
        'FORBIDDEN',
        this.view.host?.reason || 'Open an SSH window on the controller host to manage its service',
      );
  }

  private async confirm(message: string, action: string): Promise<boolean> {
    return (await vscode.window.showWarningMessage(message, { modal: true }, action)) === action;
  }

  /** Returns true when this class owns the action. The dashboard supplies error/busy handling. */
  async handle(m: FromWebview): Promise<boolean> {
    switch (m.type) {
      case 'refreshSettings':
        await this.refresh();
        return true;
      case 'savePreferences': {
        const current = preferences();
        const c = vscode.workspace.getConfiguration('gridstator');
        for (const [key, value] of Object.entries(m.patch)) {
          if (!(key in current) || typeof value !== typeof current[key as keyof EditorPreferences])
            throw new GridstatorError('BAD_REQUEST', `Invalid preference ${key}`);
          if (
            key === 'jobHistory' &&
            (!Number.isInteger(value) || Number(value) < 10 || Number(value) > 500)
          )
            throw new GridstatorError('BAD_REQUEST', 'Job history must be from 10 to 500');
          if (key === 'unsavedFiles' && !['prompt', 'saveAll', 'ignore'].includes(String(value)))
            throw new GridstatorError('BAD_REQUEST', 'Invalid unsaved files preference');
          if (key === 'notifications' && !['all', 'failures', 'none'].includes(String(value)))
            throw new GridstatorError('BAD_REQUEST', 'Invalid notifications preference');
          const workspace =
            vscode.workspace.workspaceFolders?.length &&
            (key === 'defaultNode' || c.inspect(key)?.workspaceValue !== undefined);
          await c.update(
            key,
            value,
            workspace ? vscode.ConfigurationTarget.Workspace : vscode.ConfigurationTarget.Global,
          );
        }
        this.changed();
        return true;
      }
      case 'saveClusterSettings':
        this.requireAdmin();
        await this.state.requireClient().updateSettings(m.patch);
        await this.refresh();
        return true;
      case 'createToken': {
        this.requireAdmin();
        const result = await this.state.requireClient().createToken(m.request);
        this.post({ type: 'tokenCreated', token: result.token, info: result.info });
        await this.refresh();
        return true;
      }
      case 'revokeToken':
        this.requireAdmin();
        if (
          await this.confirm(
            'Revoke this token? Clients using it will lose access.',
            'Revoke Token',
          )
        ) {
          await this.state.requireClient().revokeToken(m.id);
          await this.refresh();
        }
        return true;
      case 'requestReleases':
        this.view.includePrereleases = m.includePrereleases;
        try {
          this.view.releases = await this.state.requireClient().listReleases(m.includePrereleases);
          this.view.releaseError = null;
        } catch (err) {
          this.view.releaseError = errorMessage(err);
          this.view.releases = [];
        }
        this.changed();
        return true;
      case 'installRelease': {
        this.requireAdmin();
        const targetClusterId = this.state.connection.clusterId;
        if (
          await this.confirm(
            `Install ${m.tag} on the controller? Its service will restart and workers will follow their update policies. Running jobs will finish first.`,
            'Install Release',
          )
        ) {
          await this.installController((client) => client.installRelease(m.tag), targetClusterId);
        }
        return true;
      }
      case 'uploadControllerBuild': {
        this.requireAdmin();
        const targetClusterId = this.state.connection.clusterId;
        const selected = await vscode.window.showOpenDialog({
          canSelectMany: false,
          filters: { 'Gridstator package': ['tgz'] },
          openLabel: 'Select Controller Package',
        });
        if (!selected?.[0]) return true;
        const file = selected[0].fsPath;
        if (
          !(await this.confirm(
            'Install this controller package? The controller will restart after its running jobs finish.',
            'Install Package',
          ))
        )
          return true;
        const hash = createHash('sha256');
        await pipeline(createReadStream(file), hash);
        const digest = hash.digest('hex');
        await this.installController(
          (client) => client.publishBuild(createReadStream(file), digest),
          targetClusterId,
        );
        return true;
      }
      case 'cancelControllerInstall':
        this.controllerInstall?.abort();
        return true;
      case 'startRollout':
        this.requireAdmin();
        if (
          (m.request.now || m.request.drainTimeoutSeconds != null) &&
          !(await this.confirm('This update can cancel running jobs. Continue?', 'Update Workers'))
        )
          return true;
        await this.state.requireClient().startUpdate(m.request);
        await this.refresh();
        return true;
      case 'cancelRollout':
        this.requireAdmin();
        await this.state.requireClient().cancelUpdate();
        await this.refresh();
        return true;
      case 'restoreLocalAccess':
        await this.local.restore();
        await this.state.connect();
        await this.refresh();
        return true;
      case 'hostService':
        await this.requireHost();
        if (
          m.action !== 'start' &&
          !(await this.confirm(
            `${m.action === 'stop' ? 'Stop' : 'Restart'} the controller service? All clients will disconnect. Running jobs must finish first.`,
            m.action === 'stop' ? 'Stop Controller' : 'Restart Controller',
          ))
        )
          return true;
        await this.local.service(m.action);
        if (m.action !== 'stop') await this.state.connect();
        await this.refresh();
        return true;
      case 'saveHostConfiguration':
        await this.requireHost();
        if (
          !(await this.confirm(
            `Apply ${m.host}:${m.port} with public URL ${m.url} and restart the controller? Existing endpoints will remain available while workers reconnect.`,
            'Apply & Restart',
          ))
        )
          return true;
        await this.local.configure(m.host, m.port, m.url);
        await this.state.connect();
        await this.refresh();
        return true;
      case 'openExtensionUpdates':
        await vscode.commands.executeCommand('workbench.extensions.action.showExtensionsWithIds', [
          this.context.extension.id,
        ]);
        return true;
      default:
        return false;
    }
  }

  private async installController(
    install: (client: GridstatorClient) => Promise<PublishBuildResponse>,
    selectedClusterId: string | null,
  ): Promise<void> {
    if (this.controllerInstall)
      throw new GridstatorError('CONFLICT', 'A controller update is already in progress');
    this.state.requireClient();
    const connection = await resolveConnection(preferences().configPath || undefined);
    if (this.controllerInstall)
      throw new GridstatorError('CONFLICT', 'A controller update is already in progress');
    const probe = new GridstatorClient(connection);
    try {
      const actual = await probe.cluster();
      if (
        actual.clusterId !== selectedClusterId ||
        this.state.connection.clusterId !== selectedClusterId
      ) {
        throw new GridstatorError(
          'CONFLICT',
          'The shared connection changed. Reconnect before installing a controller update.',
        );
      }
      if (this.controllerInstall)
        throw new GridstatorError('CONFLICT', 'A controller update is already in progress');
    } catch (err) {
      probe.close();
      throw err;
    }
    const abort = new AbortController();
    this.controllerInstall = abort;
    const controller = this.state.connection.controller;
    const drained = new Set<string>();
    const clusterId = this.state.connection.clusterId;
    let updating = false;
    const renewLease = async () => {
      await this.context.globalState.update(
        RESTORE_KEY,
        this.context.globalState
          .get<PendingRestore[]>(RESTORE_KEY, [])
          .map((p) =>
            p.owner === this.session ? { ...p, expiresAt: Date.now() + 5 * 60_000 } : p,
          ),
      );
    };
    const leaseTimer = setInterval(() => {
      void renewLease().catch(() => undefined);
    }, 30_000);
    try {
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: 'Gridstator: updating controller',
          cancellable: true,
        },
        async (progress, cancellation) => {
          const listener = cancellation.onCancellationRequested(() => abort.abort());
          try {
            if (controller) {
              const localWorkers = (await probe.listNodes()).filter((n) =>
                n.info?.machineId
                  ? n.info.machineId === controller.machineId
                  : n.info?.hostname === controller.hostname,
              );
              for (const worker of localWorkers) {
                if (
                  this.context.globalState
                    .get<PendingRestore[]>(RESTORE_KEY, [])
                    .some(
                      (p) =>
                        p.clusterId === clusterId &&
                        p.nodeId === worker.id &&
                        !p.ready &&
                        (p.expiresAt ?? 0) > Date.now() &&
                        p.owner !== this.session,
                    )
                )
                  throw new GridstatorError(
                    'CONFLICT',
                    'Another window is already preparing a controller update',
                  );
                drained.add(worker.id);
                if (worker.settings.schedulingMode === 'enabled') {
                  const pending = this.context.globalState.get<PendingRestore[]>(RESTORE_KEY, []);
                  if (!pending.some((p) => p.clusterId === clusterId && p.nodeId === worker.id)) {
                    await this.context.globalState.update(RESTORE_KEY, [
                      ...pending,
                      {
                        clusterId,
                        nodeId: worker.id,
                        owner: this.session,
                        ready: false,
                        expiresAt: Date.now() + 5 * 60_000,
                      },
                    ]);
                  }
                  await probe.updateNode(worker.id, { schedulingMode: 'draining' });
                }
              }
              for (;;) {
                if (abort.signal.aborted) return;
                if (this.state.connection.clusterId !== clusterId)
                  throw new GridstatorError(
                    'CONFLICT',
                    'The selected cluster changed while waiting. The controller update was cancelled.',
                  );
                // Keep the recovery lease alive while an update legitimately waits for long jobs.
                await renewLease();
                const remaining = (await probe.listNodes())
                  .filter((n) => drained.has(n.id))
                  .reduce((sum, n) => sum + n.activeJobs.length, 0);
                if (!remaining) break;
                progress.report({
                  message: `Waiting for ${remaining} running job${remaining === 1 ? '' : 's'} to finish…`,
                });
                await new Promise<void>((resolve) => {
                  const done = () => {
                    clearTimeout(timer);
                    abort.signal.removeEventListener('abort', done);
                    resolve();
                  };
                  const timer = setTimeout(done, 1000);
                  abort.signal.addEventListener('abort', done, { once: true });
                });
              }
            }
            if (abort.signal.aborted) return;
            if (this.state.connection.clusterId !== clusterId)
              throw new GridstatorError(
                'CONFLICT',
                'The selected cluster changed. The controller update was cancelled.',
              );
            progress.report({ message: 'Installing controller package…' });
            updating = true;
            const result = await install(probe);
            progress.report({ message: 'Waiting for the controller to reconnect…' });
            const deadline = Date.now() + 180_000;
            let unavailable = false;
            for (;;) {
              const view = await probe.cluster().catch(() => undefined);
              if (view?.build?.id === result.to) break;
              if (!view) unavailable = true;
              else if (unavailable && view.build?.id === result.from)
                throw new GridstatorError(
                  'CONFLICT',
                  `The controller rolled back to ${result.from}. Check its service logs before retrying.`,
                );
              if (Date.now() >= deadline)
                throw new GridstatorError(
                  'UNAVAILABLE',
                  'The controller has not returned yet. Worker scheduling will be restored when it reconnects.',
                );
              await new Promise((resolve) => setTimeout(resolve, 1000));
            }
            if (this.state.connection.clusterId === clusterId) await this.state.connect();
          } finally {
            listener.dispose();
          }
        },
      );
    } finally {
      clearInterval(leaseTimer);
      this.controllerInstall = undefined;
      await this.context.globalState.update(
        RESTORE_KEY,
        this.context.globalState
          .get<PendingRestore[]>(RESTORE_KEY, [])
          .map((p) => (p.owner === this.session ? { ...p, ready: true } : p)),
      );
      // Restore through the original pinned connection even if another window selected a cluster.
      for (const nodeId of drained) {
        const pending = this.context.globalState.get<PendingRestore[]>(RESTORE_KEY, []);
        if (
          !pending.some(
            (p) => p.clusterId === clusterId && p.nodeId === nodeId && p.owner === this.session,
          )
        )
          continue;
        try {
          const node = await probe.getNode(nodeId);
          if (node.settings.schedulingMode === 'draining')
            await probe.updateNode(nodeId, { schedulingMode: 'enabled' });
          await this.context.globalState.update(
            RESTORE_KEY,
            this.context.globalState
              .get<PendingRestore[]>(RESTORE_KEY, [])
              .filter((p) => p.clusterId !== clusterId || p.nodeId !== nodeId),
          );
        } catch {
          /* Keep durable restoration for the next connection to this cluster. */
        }
      }
      probe.close();
      // This durable record also survives a closed window or an unusually slow restart.
      if (updating && !this.state.connected) void this.state.connect();
      await this.restorePending();
      await this.refresh();
    }
  }

  private restorePending(): Promise<void> {
    if (this.restoring) return this.restoring;
    if (this.controllerInstall || !this.state.connected) return Promise.resolve();
    this.restoring = this.restorePendingOnce().finally(() => {
      this.restoring = undefined;
    });
    return this.restoring;
  }

  private async restorePendingOnce(): Promise<void> {
    const clusterId = this.state.connection.clusterId;
    const client = this.state.client;
    if (!clusterId || !client) return;
    const pending = this.context.globalState.get<PendingRestore[]>(RESTORE_KEY, []);
    const completed = new Set<string>();
    try {
      const nodes = await client.listNodes();
      for (const item of pending.filter(
        (p) => p.clusterId === clusterId && (p.ready || (p.expiresAt ?? 0) < Date.now()),
      )) {
        const node = nodes.find((n) => n.id === item.nodeId);
        if (node?.settings.schedulingMode === 'draining')
          await client.updateNode(item.nodeId, { schedulingMode: 'enabled' });
        completed.add(item.nodeId);
      }
      await this.context.globalState.update(
        RESTORE_KEY,
        this.context.globalState
          .get<PendingRestore[]>(RESTORE_KEY, [])
          .filter((p) => p.clusterId !== clusterId || !completed.has(p.nodeId)),
      );
    } catch {
      // Keep the record and retry on the next authenticated connection.
    }
  }

  dispose(): void {
    this.generation++;
    this.onConnection.dispose();
    this.controllerInstall?.abort();
  }
}

export function settingsBusyKey(message: FromWebview): string | undefined {
  const keys: Record<string, string> = {
    refreshSettings: 'settings:refresh',
    savePreferences: 'settings:preferences',
    saveClusterSettings: 'settings:cluster',
    createToken: 'settings:tokens',
    revokeToken: 'settings:tokens',
    requestReleases: 'settings:releases',
    installRelease: 'settings:install',
    uploadControllerBuild: 'settings:install',
    startRollout: 'settings:rollout',
    cancelRollout: 'settings:rollout',
    cancelControllerInstall: 'settings:cancelInstall',
    hostService: 'settings:host',
    saveHostConfiguration: 'settings:host',
    restoreLocalAccess: 'settings:restore',
    openExtensionUpdates: 'settings:extension',
  };
  return keys[message.type];
}

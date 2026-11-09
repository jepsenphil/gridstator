import { randomBytes } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { pipeline } from 'node:stream/promises';
import * as vscode from 'vscode';
import { GridstatorError, errorMessage } from '@gridstator/shared';
import type { LogEntry } from '@gridstator/protocol';
import { CancelledByUser, type Launcher } from './launcher';
import type { DashboardState, FromWebview, ToWebview, View } from './messages';
import { mapRemotePath } from './problems';
import type { ClusterState } from './state';

export class Dashboard implements vscode.Disposable {
  private panel: vscode.WebviewPanel | undefined;
  private ready = false;
  private pending: ToWebview[] = [];
  private stateTimer: NodeJS.Timeout | undefined;
  private logAbort: AbortController | undefined;
  private selectedJob: string | null = null;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly state: ClusterState,
    private readonly launcher: Launcher,
    private readonly log: vscode.OutputChannel,
  ) {
    this.disposables.push(
      state.onDidChange(() => this.scheduleState()),
      launcher.onDidChange(() => this.scheduleState()),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('gridstator.defaultNode')) this.scheduleState();
      }),
    );
  }

  /** Opens (or reveals) the dashboard and optionally navigates to a view. */
  show(
    view: View = 'overview',
    target: { jobId?: string; nodeId?: string; itemId?: string } = {},
  ): void {
    if (!this.panel) this.create();
    else this.panel.reveal(vscode.ViewColumn.Active);
    this.post({ type: 'navigate', view, ...target });
  }

  private create(): void {
    const media = vscode.Uri.joinPath(this.context.extensionUri, 'media');
    const panel = vscode.window.createWebviewPanel(
      'gridstator.dashboard',
      'Gridstator',
      vscode.ViewColumn.Active,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [media],
      },
    );
    panel.iconPath = vscode.Uri.joinPath(media, 'gridstator.svg');
    this.panel = panel;
    this.ready = false;
    const nonce = randomBytes(16).toString('base64');
    const asset = (p: string) =>
      panel.webview.asWebviewUri(vscode.Uri.joinPath(media, p)).toString();
    panel.webview.html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${panel.webview.cspSource} data:; font-src ${panel.webview.cspSource}; style-src ${panel.webview.cspSource}; script-src 'nonce-${nonce}';" />
<link rel="stylesheet" href="${asset('codicons/codicon.css')}" />
<link rel="stylesheet" href="${asset('dashboard.css')}" />
<title>Gridstator</title>
</head>
<body>
<div id="root"></div>
<script nonce="${nonce}" src="${asset('dashboard.js')}"></script>
</body>
</html>`;
    panel.webview.onDidReceiveMessage(
      (m: FromWebview) => void this.handle(m),
      undefined,
      this.disposables,
    );
    panel.onDidDispose(() => {
      this.panel = undefined;
      this.ready = false;
      this.pending = [];
      this.logAbort?.abort();
      this.selectedJob = null;
    });
  }

  private post(msg: ToWebview): void {
    if (!this.panel) return;
    if (!this.ready) {
      this.pending.push(msg);
      return;
    }
    void this.panel.webview.postMessage(msg);
  }

  snapshot(): DashboardState {
    return {
      connection: this.state.connection,
      nodes: this.state.nodeList(),
      jobs: this.state.jobList(),
      launcher: {
        items: this.launcher.items,
        defaultNode: this.launcher.defaultNode(),
        loading: this.launcher.loading,
      },
      myJobIds: [...this.state.myJobs],
    };
  }

  private scheduleState(): void {
    if (!this.panel || this.stateTimer) return;
    this.stateTimer = setTimeout(() => {
      this.stateTimer = undefined;
      this.post({ type: 'state', state: this.snapshot() });
    }, 150);
  }

  private toast(level: 'info' | 'error', message: string): void {
    this.post({ type: 'toast', level, message });
  }

  private async guarded(key: string, fn: () => Promise<void>): Promise<void> {
    this.post({ type: 'busy', key, busy: true });
    try {
      await fn();
    } catch (err) {
      if (err instanceof CancelledByUser) return;
      const message =
        err instanceof GridstatorError && err.hint
          ? `${err.message}. ${err.hint}`
          : errorMessage(err);
      this.toast('error', message);
      this.log.appendLine(`Dashboard action ${key} failed: ${message}`);
    } finally {
      this.post({ type: 'busy', key, busy: false });
    }
  }

  private async handle(m: FromWebview): Promise<void> {
    switch (m.type) {
      case 'ready': {
        this.ready = true;
        void this.panel?.webview.postMessage({
          type: 'state',
          state: this.snapshot(),
        } satisfies ToWebview);
        for (const p of this.pending.splice(0)) void this.panel?.webview.postMessage(p);
        if (this.selectedJob) void this.streamLogs(this.selectedJob);
        return;
      }
      case 'selectJob':
        if (m.jobId === this.selectedJob) return;
        this.selectedJob = m.jobId;
        this.logAbort?.abort();
        if (m.jobId) void this.streamLogs(m.jobId);
        return;
      case 'cancel':
        return this.guarded(`cancel:${m.jobId}`, async () => {
          this.state.upsertJob(await this.state.requireClient().cancelJob(m.jobId));
        });
      case 'retry':
        return this.guarded(`retry:${m.jobId}`, async () => {
          const job = await this.state.requireClient().retryJob(m.jobId);
          this.state.myJobs.add(job.id);
          const origin = this.launcher.origins.get(m.jobId);
          if (origin) this.launcher.origins.set(job.id, origin);
          this.state.upsertJob(job);
          this.post({ type: 'navigate', view: 'job', jobId: job.id });
        });
      case 'rerun':
        return this.guarded(`rerun:${m.jobId}`, async () => {
          const job = await this.launcher.rerun(m.jobId);
          this.post({ type: 'navigate', view: 'job', jobId: job.id });
        });
      case 'downloadArtifact':
        return this.guarded(`artifact:${m.jobId}:${m.name}`, async () => {
          const base =
            vscode.workspace.workspaceFolders?.[0]?.uri ?? vscode.Uri.file(process.cwd());
          const target = await vscode.window.showSaveDialog({
            defaultUri: vscode.Uri.joinPath(base, m.name.split('/').pop() ?? 'artifact'),
            saveLabel: 'Save Artifact',
          });
          if (!target) return;
          const res = await this.state.requireClient().downloadArtifact(m.jobId, m.name);
          await pipeline(res, createWriteStream(target.fsPath));
          const choice = await vscode.window.showInformationMessage(`Saved ${m.name}`, 'Reveal');
          if (choice === 'Reveal') await vscode.commands.executeCommand('revealFileInOS', target);
        });
      case 'updateNode':
        return this.guarded(`node:${m.nodeId}`, async () => {
          const node = await this.state.requireClient().updateNode(m.nodeId, m.patch);
          this.state.upsertNode(node);
          this.toast('info', `Updated ${node.name}`);
        });
      case 'launch':
        return this.guarded(`launch:${m.request.itemId}`, async () => {
          const job = await this.launcher.launch(m.request);
          this.post({ type: 'navigate', view: 'job', jobId: job.id });
        });
      case 'setDefaultNode':
        await vscode.workspace
          .getConfiguration('gridstator')
          .update('defaultNode', m.node, vscode.ConfigurationTarget.Workspace);
        return;
      case 'refresh':
        await Promise.all([this.state.connect(), this.launcher.refresh()]);
        return;
      case 'signIn':
        await vscode.commands.executeCommand('gridstator.signIn');
        return;
      case 'openFile': {
        const origin = this.launcher.origins.get(m.jobId);
        const root = origin?.root ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        if (!root) return;
        const file = mapRemotePath(m.path, {
          root,
          workdir: origin?.workdir ?? '.',
          jobId: m.jobId,
        });
        if (!file) {
          this.toast('error', `${m.path} is not a file in this workspace`);
          return;
        }
        const pos = new vscode.Position(
          Math.max(0, (m.line ?? 1) - 1),
          Math.max(0, (m.column ?? 1) - 1),
        );
        await vscode.window.showTextDocument(vscode.Uri.file(file), {
          selection: new vscode.Range(pos, pos),
          viewColumn: vscode.ViewColumn.Beside,
        });
        return;
      }
      case 'copy':
        await vscode.env.clipboard.writeText(m.text);
        this.toast('info', 'Copied');
        return;
    }
  }

  /** Streams a job's logs (backlog, then live) to the webview until it ends or another job is selected. */
  private async streamLogs(jobId: string): Promise<void> {
    const client = this.state.client;
    if (!client) return;
    const abort = new AbortController();
    this.logAbort = abort;
    let buffer: LogEntry[] = [];
    let first = true;
    const flush = () => {
      if (buffer.length === 0 && !first) return;
      this.post({ type: 'logs', jobId, entries: buffer, reset: first });
      first = false;
      buffer = [];
    };
    const timer = setInterval(flush, 100);
    try {
      void client
        .jobEvents(jobId)
        .then((events) => this.post({ type: 'events', jobId, events }))
        .catch(() => undefined);
      const state = await client.followLogs(jobId, (entries) => buffer.push(...entries), {
        signal: abort.signal,
      });
      flush();
      this.post({ type: 'logs.end', jobId, state });
      const events = await client.jobEvents(jobId).catch(() => undefined);
      if (events) this.post({ type: 'events', jobId, events });
    } catch (err) {
      if (!abort.signal.aborted) {
        flush();
        this.toast('error', `Log stream for ${jobId}: ${errorMessage(err)}`);
      }
    } finally {
      clearInterval(timer);
    }
  }

  dispose(): void {
    if (this.stateTimer) clearTimeout(this.stateTimer);
    this.logAbort?.abort();
    this.panel?.dispose();
    for (const d of this.disposables) d.dispose();
  }
}

import * as vscode from 'vscode';
import { formatBytes, formatDuration } from '@gridstator/shared';
import { isTerminal, type Job, type Node } from '@gridstator/protocol';
import type { LaunchItem } from './messages';
import type { Launcher } from './launcher';
import type { ClusterState } from './state';

function jobIcon(job: Job): vscode.ThemeIcon {
  switch (job.state) {
    case 'succeeded':
      return new vscode.ThemeIcon('pass-filled', new vscode.ThemeColor('testing.iconPassed'));
    case 'failed':
      return new vscode.ThemeIcon('error', new vscode.ThemeColor('testing.iconFailed'));
    case 'lost':
      return new vscode.ThemeIcon('warning', new vscode.ThemeColor('list.warningForeground'));
    case 'cancelled':
      return new vscode.ThemeIcon('circle-slash', new vscode.ThemeColor('disabledForeground'));
    case 'running':
    case 'starting':
    case 'assigned':
      return new vscode.ThemeIcon('sync~spin', new vscode.ThemeColor('charts.blue'));
    default:
      return new vscode.ThemeIcon('clock', new vscode.ThemeColor('disabledForeground'));
  }
}

export function jobDescription(job: Job, now = Date.now()): string {
  const parts: string[] = [job.state];
  if (job.shardIndex !== null) parts.unshift(`shard ${job.shardIndex}/${job.shardTotal}`);
  if (job.state === 'queued') parts.push(job.stateReason ?? `#${job.queuePosition ?? '?'}`);
  else if (job.nodeName) parts.push(job.nodeName);
  if (job.startedAt) parts.push(formatDuration((job.finishedAt ?? now) - job.startedAt));
  return parts.join(' · ');
}

// ---- cluster ---------------------------------------------------------------------------------

type ClusterElement = Node | { kind: 'controller' };

export class ClusterTreeProvider implements vscode.TreeDataProvider<ClusterElement> {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.emitter.event;

  constructor(private readonly state: ClusterState) {
    state.onDidChange(() => this.emitter.fire());
  }

  getChildren(): ClusterElement[] {
    if (!this.state.connected) return [];
    const nodes = this.state.nodeList();
    const controller = this.state.connection.controller;
    const colocated = nodes.some((n) =>
      n.info?.machineId
        ? n.info.machineId === controller?.machineId
        : n.info?.hostname === controller?.hostname,
    );
    return controller && !colocated ? [{ kind: 'controller' }, ...nodes] : nodes;
  }

  getTreeItem(n: ClusterElement): vscode.TreeItem {
    if ('kind' in n) {
      const item = new vscode.TreeItem(
        this.state.connection.controller?.hostname ?? 'Controller',
        vscode.TreeItemCollapsibleState.None,
      );
      item.id = `controller:${this.state.connection.clusterId}`;
      item.description = `controller · v${this.state.connection.version ?? '?'}`;
      item.iconPath = new vscode.ThemeIcon('server-process');
      item.contextValue = 'controller';
      item.tooltip = `${this.state.connection.url}\nController ${this.state.connection.build?.id ?? this.state.connection.version ?? 'unknown version'}`;
      item.command = { command: 'gridstator.openSettings', title: 'Controller Settings' };
      return item;
    }
    const item = new vscode.TreeItem(n.name, vscode.TreeItemCollapsibleState.None);
    const online = n.status === 'online';
    const parts: string[] = [];
    const controller = this.state.connection.controller;
    if (
      controller &&
      (n.info?.machineId
        ? n.info.machineId === controller.machineId
        : n.info?.hostname === controller.hostname)
    )
      parts.push('controller + worker');
    if (!online) parts.push('offline');
    else {
      if (n.metrics)
        parts.push(
          `${Math.round(n.metrics.cpuPercent)}% CPU`,
          `${formatBytes(n.metrics.memoryAvailableMb * 1024 * 1024)} free`,
        );
      parts.push(`${n.activeJobs.length}/${n.settings.maxConcurrency} jobs`);
      if (n.settings.schedulingMode !== 'enabled') parts.push(n.settings.schedulingMode);
    }
    const update = n.update?.state;
    if (update === 'behind' || update === 'failed' || update === 'updating')
      parts.push(update === 'behind' ? 'update pending' : `update ${update}`);
    item.description = parts.join(' · ');
    item.iconPath = !online
      ? new vscode.ThemeIcon('debug-disconnect', new vscode.ThemeColor('disabledForeground'))
      : update === 'updating'
        ? new vscode.ThemeIcon('sync~spin', new vscode.ThemeColor('list.warningForeground'))
        : update === 'behind' || update === 'failed'
          ? new vscode.ThemeIcon('cloud-download', new vscode.ThemeColor('list.warningForeground'))
          : n.health === 'degraded'
            ? new vscode.ThemeIcon('warning', new vscode.ThemeColor('list.warningForeground'))
            : n.settings.schedulingMode !== 'enabled'
              ? new vscode.ThemeIcon('debug-pause', new vscode.ThemeColor('list.warningForeground'))
              : new vscode.ThemeIcon('vm-running', new vscode.ThemeColor('testing.iconPassed'));
    item.contextValue = `node.${n.settings.schedulingMode}`;
    const md = new vscode.MarkdownString(undefined, true);
    md.appendMarkdown(
      `**${n.name}** — ${n.status}${n.health === 'degraded' ? ' (degraded)' : ''}\n\n`,
    );
    if (n.info) {
      md.appendMarkdown(
        `${n.info.platform}/${n.info.arch} · ${n.info.cpus} CPU · ${formatBytes(n.info.memoryTotalMb * 1024 * 1024)}\n\n`,
      );
      md.appendMarkdown(
        `Docker ${n.info.docker.version ?? 'unavailable'} · buildx ${n.info.buildx.available ? 'yes' : 'no'} · act ${n.info.act.available ? (n.info.act.version ?? 'yes') : 'no'}\n\n`,
      );
    }
    md.appendMarkdown(
      `Reserved for host: ${n.settings.reserved.cpus} CPU, ${formatBytes(n.settings.reserved.memoryMb * 1024 * 1024)}\n\n`,
    );
    if (n.effectiveLabels.length)
      md.appendMarkdown(`Labels: ${n.effectiveLabels.map((l) => `\`${l}\``).join(' ')}\n\n`);
    for (const issue of n.healthIssues) md.appendMarkdown(`$(warning) ${issue}\n\n`);
    if (n.update) {
      md.appendMarkdown(
        `Build ${n.update.build ?? 'unknown'} — ${n.update.state}${n.update.detail ? `: ${n.update.detail}` : ''} (auto-update ${n.update.autoUpdate})\n\n`,
      );
    }
    item.tooltip = md;
    item.command = { command: 'gridstator.node.open', title: 'Manage Node', arguments: [n] };
    item.id = `node:${n.id}`;
    return item;
  }
}

// ---- jobs ------------------------------------------------------------------------------------

type JobsElement = { group: 'active' | 'queued' | 'recent'; label: string } | Job;
const isGroup = (e: JobsElement): e is { group: 'active' | 'queued' | 'recent'; label: string } =>
  'group' in e;

export class JobsTreeProvider implements vscode.TreeDataProvider<JobsElement> {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.emitter.event;
  private timer: NodeJS.Timeout | undefined;

  constructor(private readonly state: ClusterState) {
    state.onDidChange(() => this.emitter.fire());
    // Durations of running jobs tick while visible.
    this.timer = setInterval(() => {
      if (this.state.jobList().some((j) => j.state === 'running')) this.emitter.fire();
    }, 2000);
  }

  getChildren(element?: JobsElement): JobsElement[] {
    if (!this.state.connected) return [];
    const jobs = this.state.jobList();
    if (!element) {
      const active = jobs.filter((j) =>
        ['assigned', 'starting', 'running'].includes(j.state),
      ).length;
      const queued = jobs.filter((j) => j.state === 'queued' || j.state === 'created').length;
      const groups: JobsElement[] = [];
      if (active) groups.push({ group: 'active', label: `Running (${active})` });
      if (queued) groups.push({ group: 'queued', label: `Queued (${queued})` });
      if (jobs.some((j) => isTerminal(j.state))) groups.push({ group: 'recent', label: 'Recent' });
      return groups;
    }
    if (!isGroup(element)) return [];
    if (element.group === 'active')
      return jobs.filter((j) => ['assigned', 'starting', 'running'].includes(j.state));
    if (element.group === 'queued')
      return jobs.filter((j) => j.state === 'queued' || j.state === 'created');
    return jobs.filter((j) => isTerminal(j.state)).slice(0, 30);
  }

  getTreeItem(e: JobsElement): vscode.TreeItem {
    if (isGroup(e)) {
      const item = new vscode.TreeItem(e.label, vscode.TreeItemCollapsibleState.Expanded);
      item.id = `group:${e.group}`;
      return item;
    }
    const item = new vscode.TreeItem(e.name, vscode.TreeItemCollapsibleState.None);
    item.id = `job:${e.id}`;
    item.description = jobDescription(e);
    item.iconPath = jobIcon(e);
    item.contextValue = isTerminal(e.state) ? 'job.done' : 'job.active';
    item.tooltip = new vscode.MarkdownString(
      `**${e.name}**\n\n${e.kind} · ${e.state}${e.error ? `\n\n${e.error}` : ''}\n\nJob \`${e.id}\`${this.state.myJobs.has(e.id) ? ' · started from this window' : ` · by ${e.owner}`}`,
    );
    item.command = { command: 'gridstator.job.open', title: 'Show Job', arguments: [e] };
    return item;
  }

  dispose(): void {
    if (this.timer) clearInterval(this.timer);
  }
}

// ---- launcher --------------------------------------------------------------------------------

type LauncherElement =
  | { section: string; label: string; icon: string }
  | { item: LaunchItem; job?: { id: string; name: string; runnable: boolean; warnings: string[] } };

const SECTIONS: Array<{ kind: LaunchItem['kind']; label: string; icon: string }> = [
  { kind: 'task', label: 'Tasks', icon: 'checklist' },
  { kind: 'build', label: 'Builds', icon: 'package' },
  { kind: 'workflow', label: 'GitHub Actions', icon: 'github-action' },
  { kind: 'script', label: 'Package Scripts', icon: 'symbol-event' },
  { kind: 'dockerfile', label: 'Dockerfiles', icon: 'file-code' },
];

export class LauncherTreeProvider implements vscode.TreeDataProvider<LauncherElement> {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.emitter.event;

  constructor(private readonly launcher: Launcher) {
    launcher.onDidChange(() => this.emitter.fire());
  }

  getChildren(element?: LauncherElement): LauncherElement[] {
    if (!element) {
      return SECTIONS.filter((s) => this.launcher.items.some((i) => i.kind === s.kind)).map(
        (s) => ({ section: s.kind, label: s.label, icon: s.icon }),
      );
    }
    if ('section' in element)
      return this.launcher.items
        .filter((i) => i.kind === element.section)
        .map((item) => ({ item }));
    if (element.item.kind === 'workflow' && !element.job) {
      return (element.item.workflow?.jobs ?? []).map((job) => ({ item: element.item, job }));
    }
    return [];
  }

  getTreeItem(e: LauncherElement): vscode.TreeItem {
    if ('section' in e) {
      const t = new vscode.TreeItem(e.label, vscode.TreeItemCollapsibleState.Expanded);
      t.iconPath = new vscode.ThemeIcon(e.icon);
      t.id = `section:${e.section}`;
      return t;
    }
    const multiRoot = (vscode.workspace.workspaceFolders?.length ?? 0) > 1;
    if (e.job) {
      const t = new vscode.TreeItem(
        e.job.name === e.job.id ? e.job.id : `${e.job.id} — ${e.job.name}`,
        vscode.TreeItemCollapsibleState.None,
      );
      t.id = `${e.item.id}#${e.job.id}`;
      t.iconPath = new vscode.ThemeIcon(
        e.job.runnable ? (e.job.warnings.length ? 'warning' : 'circle-outline') : 'circle-slash',
      );
      t.description = e.job.runnable
        ? e.job.warnings.length
          ? 'with caveats'
          : ''
        : 'cannot run locally';
      if (e.job.warnings.length) t.tooltip = e.job.warnings.join('\n');
      t.contextValue = e.job.runnable ? 'launch.workflowJob' : 'launchDisabled';
      t.command = {
        command: 'gridstator.openRun',
        title: 'Configure',
        arguments: [e.item.id, e.job.id],
      };
      return t;
    }
    const item = e.item;
    const t = new vscode.TreeItem(
      item.label,
      item.kind === 'workflow'
        ? vscode.TreeItemCollapsibleState.Collapsed
        : vscode.TreeItemCollapsibleState.None,
    );
    t.id = item.id;
    t.description = `${item.description ?? ''}${multiRoot ? ` · ${item.folder}` : ''}`;
    t.contextValue = item.id.startsWith('error:') ? 'launchError' : `launch.${item.kind}`;
    t.iconPath = item.warnings.length ? new vscode.ThemeIcon('warning') : undefined;
    if (item.warnings.length) t.tooltip = item.warnings.join('\n');
    t.command = { command: 'gridstator.openRun', title: 'Configure', arguments: [item.id] };
    return t;
  }
}

export type { LauncherElement };

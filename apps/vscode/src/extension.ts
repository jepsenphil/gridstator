import * as vscode from 'vscode';
import {
  GridstatorError,
  defaultClientConfigPath,
  errorMessage,
  formatDuration,
} from '@gridstator/shared';
import {
  isTerminal,
  NodeNameSchema,
  type Job,
  type Node,
  type PatchNodeRequest,
} from '@gridstator/protocol';
import {
  GridstatorClient,
  fetchPinnedCa,
  normalizeControllerUrl,
  writeClientConfig,
  watchClientConfig,
  readClientConfig,
} from '@gridstator/client';
import { Dashboard } from './dashboard';
import { CancelledByUser, Launcher, isGroup } from './launcher';
import type { LaunchItem, LaunchOptions } from './messages';
import { ResultsIntegration } from './results';
import { ClusterState } from './state';
import { LocalController } from './local-controller';
import { effectiveClientConfigPath, NativeCancelledError, NativeCli } from './native';
import { promptConnection } from './sign-in';
import { canSetUpController, setUpController, usesDefaultConnection } from './setup';
import {
  ClusterTreeProvider,
  JobsTreeProvider,
  LauncherTreeProvider,
  type LauncherElement,
} from './views';

export interface GridstatorApi {
  state: ClusterState;
  launcher: Launcher;
  results: ResultsIntegration;
  dashboard: Dashboard;
}

function config<T>(key: string, fallback: T): T {
  return vscode.workspace.getConfiguration('gridstator').get<T>(key, fallback);
}

function showError(err: unknown): void {
  if (err instanceof CancelledByUser || err instanceof NativeCancelledError) return;
  const message =
    err instanceof GridstatorError && err.hint ? `${err.message}. ${err.hint}` : errorMessage(err);
  void vscode.window.showErrorMessage(`Gridstator: ${message}`);
}

function command(id: string, fn: (...args: never[]) => unknown): vscode.Disposable {
  return vscode.commands.registerCommand(id, async (...args: never[]) => {
    try {
      await fn(...args);
    } catch (err) {
      showError(err);
    }
  });
}

export async function activate(context: vscode.ExtensionContext): Promise<GridstatorApi> {
  const log = vscode.window.createOutputChannel('Gridstator');
  const native = new NativeCli();
  const configPath = () =>
    native.enabled ? effectiveClientConfigPath() : config<string>('configPath', '') || undefined;
  const local = new LocalController(context, configPath, log, native);
  let settingUp = false;
  let setupTask: Promise<void> | undefined;
  let canSetUp = false;
  const state = new ClusterState(
    configPath,
    () => config<number>('jobHistory', 100),
    log,
    async () => {
      if (settingUp) return;
      if (config<boolean>('autoConnect', true)) {
        try {
          await local.prepare();
        } catch (err) {
          log.appendLine(`Automatic local sign-in: ${errorMessage(err)}`);
        }
      }
      canSetUp = await canSetUpController(config<string>('configPath', ''), configPath());
      if (
        local.matchesLocal &&
        !local.canRestore &&
        local.host?.setupPending &&
        usesDefaultConnection(config<string>('configPath', ''))
      )
        canSetUp = true;
      void vscode.commands.executeCommand('setContext', 'gridstator.canSetUpController', canSetUp);
      void vscode.commands.executeCommand(
        'setContext',
        'gridstator.canStartController',
        local.canStart,
      );
    },
    (authenticationFailed) => ({
      canRestoreLocalAccess:
        local.canRestore || Boolean(authenticationFailed && local.matchesLocal),
      publicUrl: local.matchesLocal ? local.host?.url : undefined,
      canStartLocalController: local.canStart,
      canSetUpController: canSetUp,
      setupPending: Boolean(local.host?.setupPending && (local.matchesLocal || canSetUp)),
    }),
  );
  const launcher = new Launcher(state, log, native);
  const results = new ResultsIntegration(state, launcher, log);
  const dashboard = new Dashboard(context, state, launcher, log, local);
  dashboard.results = results;
  const jobsTree = new JobsTreeProvider(state);
  const clusterTree = new ClusterTreeProvider(state);
  context.subscriptions.push(
    log,
    native,
    state,
    launcher,
    results,
    dashboard,
    jobsTree,
    clusterTree,
    vscode.window.registerTreeDataProvider('gridstator.cluster', clusterTree),
    vscode.window.registerTreeDataProvider('gridstator.jobs', jobsTree),
    vscode.window.registerTreeDataProvider(
      'gridstator.launcher',
      new LauncherTreeProvider(launcher),
    ),
  );

  // ---- status bar ------------------------------------------------------------------------------
  const status = vscode.window.createStatusBarItem(
    'gridstator.status',
    vscode.StatusBarAlignment.Left,
    10,
  );
  status.name = 'Gridstator';
  const renderStatus = () => {
    const c = state.connection;
    if (c.status === 'signedOut') {
      status.text = c.canSetUpController
        ? '$(server-environment) Gridstator: Set up'
        : '$(server-environment) Gridstator: Sign in';
      status.command = c.canSetUpController ? 'gridstator.openDashboard' : 'gridstator.signIn';
      status.tooltip = 'Connect to a Gridstator controller';
      status.backgroundColor = undefined;
    } else if (c.status === 'error' || c.status === 'connecting') {
      status.text =
        c.status === 'connecting' ? '$(sync~spin) Gridstator' : '$(warning) Gridstator: offline';
      status.command = 'gridstator.refresh';
      status.tooltip = c.error ?? 'Connecting…';
      status.backgroundColor =
        c.status === 'error' ? new vscode.ThemeColor('statusBarItem.warningBackground') : undefined;
    } else {
      const nodes = state.nodeList();
      const online = nodes.filter((n) => n.status === 'online').length;
      const running = state
        .jobList()
        .filter((j) => ['assigned', 'starting', 'running'].includes(j.state)).length;
      const queued = state.jobList().filter((j) => j.state === 'queued').length;
      status.text = `$(server-environment) ${online}/${nodes.length}${running ? `  $(sync~spin) ${running}` : ''}${queued ? `  $(clock) ${queued}` : ''}`;
      status.command = 'gridstator.openDashboard';
      const target = config<string>('defaultNode', '');
      status.tooltip = `Gridstator: ${online} of ${nodes.length} nodes online, ${running} running, ${queued} queued\nController: ${c.controller?.hostname ?? c.url}\nVersion: ${c.version ?? 'unknown'}\nTarget: ${target || 'automatic'}\n${c.url} (${c.user})`;
      status.backgroundColor = undefined;
    }
    status.show();
  };
  context.subscriptions.push(status, state.onDidChange(renderStatus));
  renderStatus();

  // ---- notifications ---------------------------------------------------------------------------
  context.subscriptions.push(
    state.onJobFinished((job) => {
      // Shards are reported once, when their group finishes.
      if (!state.myJobs.has(job.id) || job.groupId) return;
      const mode = config<string>('notifications', 'failures');
      const ok = job.state === 'succeeded';
      if (mode === 'none' || (mode === 'failures' && ok)) return;
      const took =
        job.startedAt && job.finishedAt
          ? ` in ${formatDuration(job.finishedAt - job.startedAt)}`
          : '';
      const text = `${job.name} ${job.state} on ${job.nodeName ?? 'the cluster'}${took}${job.error && !ok ? `: ${job.error}` : ''}`;
      const actions = ok ? ['Show Details'] : ['Show Details', 'Retry'];
      const show = ok
        ? vscode.window.showInformationMessage
        : job.state === 'cancelled'
          ? vscode.window.showInformationMessage
          : vscode.window.showErrorMessage;
      void show(`Gridstator: ${text}`, ...actions).then(async (choice) => {
        if (choice === 'Show Details') dashboard.show('job', { jobId: job.id });
        if (choice === 'Retry') {
          try {
            const next = await state.requireClient().retryJob(job.id);
            state.myJobs.add(next.id);
            const origin = launcher.origins.get(job.id);
            if (origin) launcher.origins.set(next.id, origin);
            state.upsertJob(next);
            dashboard.show('job', { jobId: next.id });
          } catch (err) {
            showError(err);
          }
        }
      });
    }),
    state.onGroupFinished((group) => {
      if (!state.myGroups.has(group.id)) return;
      const mode = config<string>('notifications', 'failures');
      const ok = group.state === 'succeeded';
      if (mode === 'none' || (mode === 'failures' && ok)) return;
      const took =
        group.startedAt && group.finishedAt
          ? ` in ${formatDuration(group.finishedAt - group.startedAt)}`
          : '';
      const failed = group.jobs.filter((j) => j.state === 'failed' || j.state === 'lost').length;
      const text = `${group.name}: ${group.shardTotal} shards ${group.state} on ${group.nodes.join(', ')}${took}${failed ? ` (${failed} failed)` : ''}`;
      const actions =
        ok || group.state === 'cancelled'
          ? ['Show Details']
          : ['Show Details', 'Retry Failed Shards'];
      const show =
        ok || group.state === 'cancelled'
          ? vscode.window.showInformationMessage
          : vscode.window.showErrorMessage;
      void show(`Gridstator: ${text}`, ...actions).then(async (choice) => {
        if (choice === 'Show Details') dashboard.show('group', { groupId: group.id });
        if (choice === 'Retry Failed Shards') {
          try {
            const next = await state.requireClient().retryGroup(group.id);
            const origin = launcher.groupOrigins.get(group.id);
            for (const j of next.jobs) {
              state.myJobs.add(j.id);
              if (origin) launcher.origins.set(j.id, origin);
              state.upsertJob(j);
            }
            state.upsertGroup(next);
            dashboard.show('group', { groupId: group.id });
          } catch (err) {
            showError(err);
          }
        }
      });
    }),
  );

  // ---- helpers ---------------------------------------------------------------------------------
  let lastJob: string | undefined;
  const launchAndShow = async (itemId: string, options: LaunchOptions = {}) => {
    const launched = await launcher.launch({ itemId, options });
    lastJob = isGroup(launched) ? launched.jobs[0]?.id : launched.id;
    dashboard.show(
      isGroup(launched) ? 'group' : 'job',
      isGroup(launched) ? { groupId: launched.id } : { jobId: launched.id },
    );
    return launched;
  };
  const pickItem = async (
    kind: LaunchItem['kind'],
    placeHolder: string,
  ): Promise<LaunchItem | undefined> => {
    if (launcher.items.length === 0) await launcher.refresh();
    const items = launcher.items.filter((i) => i.kind === kind);
    if (items.length === 0) {
      void vscode.window.showInformationMessage(
        `Gridstator: nothing of that kind found in this workspace.`,
      );
      return undefined;
    }
    const picked = await vscode.window.showQuickPick(
      items.map((i) => ({
        label: i.label,
        description: i.description,
        detail: i.warnings.join(' · ') || undefined,
        item: i,
      })),
      { placeHolder, matchOnDescription: true },
    );
    return picked?.item;
  };
  const nodeArg = (n: Node | undefined): Node => {
    if (!n) throw new GridstatorError('BAD_REQUEST', 'Select a node in the Cluster view');
    return state.nodes.get(n.id) ?? n;
  };
  const updateNode = async (n: Node, patch: PatchNodeRequest) => {
    state.upsertNode(await state.requireClient().updateNode(n.id, patch));
  };

  // ---- commands --------------------------------------------------------------------------------
  let stopWatching = () => {};
  let configTimer: NodeJS.Timeout | undefined;
  const onCredentialsChanged = () => {
    if (settingUp) return;
    if (configTimer) clearTimeout(configTimer);
    configTimer = setTimeout(() => {
      if (settingUp) return;
      if (native.enabled) native.reset();
      if (config<boolean>('autoConnect', true)) {
        local.reset();
        void state.connect();
      }
    }, 150);
  };
  const watchCredentials = () => {
    stopWatching();
    stopWatching = watchClientConfig(
      configPath() ?? defaultClientConfigPath(),
      onCredentialsChanged,
    );
  };
  watchCredentials();
  context.subscriptions.push({
    dispose: () => {
      stopWatching();
      if (configTimer) clearTimeout(configTimer);
    },
  });
  context.subscriptions.push(
    command('gridstator.openDashboard', () => dashboard.show('overview')),
    command('gridstator.generateJoinCommand', () => dashboard.show('addWorker')),
    command('gridstator.setupController', async () => {
      if (setupTask) return setupTask;
      setupTask = (async () => {
        // Credential/configuration watchers resume only after the private setup request exits.
        settingUp = true;
        if (configTimer) clearTimeout(configTimer);
        try {
          if (!native.enabled && (await readClientConfig(configPath())))
            throw new GridstatorError('CONFIG', 'A saved connection is already selected', {
              hint: 'Use the existing connection before setting up another controller.',
            });
          const result = await setUpController(context, native);
          if (result.persistenceWarning)
            void vscode.window.showWarningMessage(`Gridstator: ${result.persistenceWarning}`);
          if (result.workerWaitingForDocker)
            void vscode.window.showInformationMessage(
              'Gridstator: controller ready. The local worker is waiting for Docker; start Docker to run jobs here.',
            );
          dashboard.show('overview');
        } finally {
          settingUp = false;
          native.reset();
          local.reset();
          watchCredentials();
          await state.connect();
          await launcher.refresh();
        }
      })().finally(() => {
        setupTask = undefined;
      });
      return setupTask;
    }),
    command('gridstator.startController', async () => {
      await local.prepare();
      if (!local.canStart)
        throw new GridstatorError(
          'CONFIG',
          'No stopped controller service owned by this account was found',
        );
      await local.service('start');
      await state.connect();
    }),
    command('gridstator.openSettings', () => dashboard.show('settings')),
    command('gridstator.restoreLocalAccess', async () => {
      await local.restore();
      await state.connect();
    }),
    command('gridstator.openRun', (itemId?: string, job?: string) =>
      dashboard.show('run', itemId ? { itemId: job ? `${itemId}#${job}` : itemId } : {}),
    ),
    command('gridstator.refresh', async () => {
      await Promise.all([state.connect(), launcher.refresh()]);
    }),
    command('gridstator.signIn', async () => {
      if (settingUp)
        throw new GridstatorError(
          'CONFLICT',
          'Wait for controller setup to finish before changing the connection',
        );
      const details = await promptConnection();
      if (!details) return;
      const { url, fingerprint, token } = details;
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: 'Gridstator: verifying the controller…',
        },
        async () => {
          const base = normalizeControllerUrl(url);
          const ca = await fetchPinnedCa(base, fingerprint);
          const client = new GridstatorClient({ url: base, ca, token });
          try {
            const cluster = await client.cluster();
            await writeClientConfig(
              {
                version: 1,
                url: base,
                caPem: ca,
                caFingerprint: cluster.caFingerprint,
                token,
                tokenName: cluster.whoami.name,
              },
              configPath() ?? defaultClientConfigPath(),
            );
            void vscode.window.showInformationMessage(
              `Gridstator: signed in to ${base} as ${cluster.whoami.name}. The CLI on this machine uses the same connection.`,
            );
          } finally {
            client.close();
          }
        },
      );
      await state.connect();
    }),
    command('gridstator.runCommand', async () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) throw new GridstatorError('BAD_REQUEST', 'Open a folder first');
      const cmd = await vscode.window.showInputBox({
        prompt: 'Command to run in a container on the cluster (from the workspace root)',
        placeHolder: 'npm ci && npm test',
        ignoreFocusOut: true,
      });
      if (!cmd) return;
      await launchAndShow(`command:${folder.name}`, { command: cmd });
    }),
    command('gridstator.runTask', async () => {
      const item = await pickItem('task', 'Task from gridstator.yml');
      if (item) await launchAndShow(item.id);
    }),
    command('gridstator.runScript', async () => {
      const item = await pickItem('script', 'Package script to run on the cluster');
      if (item) await launchAndShow(item.id);
    }),
    command('gridstator.runWorkflow', async () => {
      const item = await pickItem('workflow', 'GitHub Actions workflow');
      if (!item?.workflow) return;
      const jobs = item.workflow.jobs.filter((j) => j.runnable);
      const pick = await vscode.window.showQuickPick(
        [
          { label: '$(run-all) Whole workflow', id: '' },
          ...jobs.map((j) => ({
            label: j.id,
            description: j.name !== j.id ? j.name : undefined,
            detail: j.warnings.join(' · ') || undefined,
            id: j.id,
          })),
        ],
        { placeHolder: 'Run the whole workflow or one job' },
      );
      if (!pick) return;
      const events = item.workflow.events.length ? item.workflow.events : ['push'];
      const event =
        events.length === 1
          ? events[0]
          : await vscode.window.showQuickPick(events, {
              placeHolder: `Triggering event (default ${item.workflow.defaultEvent})`,
            });
      if (!event) return;
      const inputs: Record<string, string> = {};
      if (event === 'workflow_dispatch') {
        for (const input of item.workflow.inputs) {
          const value = input.options?.length
            ? await vscode.window.showQuickPick(input.options, {
                placeHolder: `${input.name}${input.description ? ` — ${input.description}` : ''}`,
              })
            : await vscode.window.showInputBox({
                prompt: `Input ${input.name}${input.description ? ` — ${input.description}` : ''}`,
                value: input.default ?? '',
              });
          if (value === undefined) return;
          if (value !== '') inputs[input.name] = value;
        }
      }
      await launchAndShow(item.id, { ...(pick.id ? { job: pick.id } : {}), event, inputs });
    }),
    command('gridstator.buildImage', async () => {
      if (launcher.items.length === 0) await launcher.refresh();
      const candidates = launcher.items.filter(
        (i) => i.kind === 'build' || i.kind === 'dockerfile',
      );
      if (candidates.length === 0)
        throw new GridstatorError('NOT_FOUND', 'No Dockerfiles or builds found in this workspace');
      const picked = await vscode.window.showQuickPick(
        candidates.map((i) => ({ label: i.label, description: i.description, item: i })),
        { placeHolder: 'What to build' },
      );
      if (!picked) return;
      const item = picked.item;
      let stage: string | undefined;
      if (item.dockerfile && item.dockerfile.stages.length > 0) {
        const s = await vscode.window.showQuickPick(['(final stage)', ...item.dockerfile.stages], {
          placeHolder: 'Build target stage',
        });
        if (s === undefined) return;
        if (s !== '(final stage)') stage = s;
      }
      const tag =
        item.kind === 'dockerfile'
          ? await vscode.window.showInputBox({
              prompt: 'Image tag (optional)',
              placeHolder: 'my-app:dev',
            })
          : undefined;
      if (tag === undefined && item.kind === 'dockerfile') return;
      await launchAndShow(item.id, {
        ...(stage ? { stage } : {}),
        ...(tag ? { tags: [tag] } : {}),
      });
    }),
    command('gridstator.rerunLast', async () => {
      const last = lastJob ?? [...state.myJobs].pop();
      if (!last)
        throw new GridstatorError('NOT_FOUND', 'Nothing has been run from this window yet');
      const launched = await launcher.rerun(last);
      lastJob = isGroup(launched) ? launched.jobs[0]?.id : launched.id;
      dashboard.show(
        isGroup(launched) ? 'group' : 'job',
        isGroup(launched) ? { groupId: launched.id } : { jobId: launched.id },
      );
    }),
    command('gridstator.selectTarget', async () => {
      const nodes = state.nodeList();
      const current = config<string>('defaultNode', '');
      const pick = await vscode.window.showQuickPick(
        [
          {
            label: '$(sparkle) Automatic',
            description: 'Let the scheduler choose the best available worker',
            value: '',
            picked: current === '',
          },
          ...nodes.map((n) => ({
            label: `${n.status === 'online' ? '$(vm-running)' : '$(debug-disconnect)'} ${n.name}`,
            description: `${n.status}${n.info ? ` · ${n.info.cpus} CPU` : ''} · ${n.activeJobs.length}/${n.settings.maxConcurrency} jobs`,
            value: n.name,
            picked: current === n.name,
          })),
        ],
        { placeHolder: `Run jobs on… (currently ${current || 'automatic'})` },
      );
      if (!pick) return;
      await vscode.workspace
        .getConfiguration('gridstator')
        .update('defaultNode', pick.value, vscode.ConfigurationTarget.Workspace);
      renderStatus();
    }),
    command('gridstator.clearProblems', () => results.clear()),
    command('gridstator.launch', async (element?: LauncherElement) => {
      if (!element || 'section' in element) return;
      const item = element.item;
      if (item.kind === 'workflow') {
        await launchAndShow(item.id, element.job ? { job: element.job.id } : {});
      } else {
        await launchAndShow(item.id);
      }
    }),
    command('gridstator.job.open', (job?: Job) =>
      dashboard.show('job', job ? { jobId: job.id } : {}),
    ),
    command('gridstator.job.cancel', async (job?: Job) => {
      if (!job) return;
      state.upsertJob(await state.requireClient().cancelJob(job.id));
    }),
    command('gridstator.job.retry', async (job?: Job) => {
      if (!job) return;
      job = state.jobs.get(job.id) ?? job;
      if (!isTerminal(job.state)) return;
      const next = await state.requireClient().retryJob(job.id);
      state.myJobs.add(next.id);
      const origin = launcher.origins.get(job.id);
      if (origin) launcher.origins.set(next.id, origin);
      state.upsertJob(next);
      dashboard.show('job', { jobId: next.id });
    }),
    command('gridstator.node.open', (n?: Node) =>
      dashboard.show('nodes', n ? { nodeId: n.id } : {}),
    ),
    command('gridstator.node.rename', async (n?: Node) => {
      const node = nodeArg(n);
      const name = await vscode.window.showInputBox({
        prompt: `Rename ${node.name}`,
        value: node.name,
        validateInput: (value) => {
          if (!NodeNameSchema.safeParse(value).success)
            return 'Use 1–64 letters, numbers, dots, underscores or hyphens; start with a letter or number';
          if (
            state
              .nodeList()
              .some((other) => other.id !== node.id && (other.name === value || other.id === value))
          )
            return 'Another worker already uses this name';
          return undefined;
        },
      });
      if (name !== undefined && name !== node.name) await updateNode(node, { name });
    }),
    command('gridstator.node.enable', (n?: Node) =>
      updateNode(nodeArg(n), { schedulingMode: 'enabled' }),
    ),
    command('gridstator.node.drain', (n?: Node) =>
      updateNode(nodeArg(n), { schedulingMode: 'draining' }),
    ),
    command('gridstator.node.disable', (n?: Node) =>
      updateNode(nodeArg(n), { schedulingMode: 'disabled' }),
    ),
    command('gridstator.node.setConcurrency', async (n?: Node) => {
      const node = nodeArg(n);
      const v = await vscode.window.showInputBox({
        prompt: `Maximum concurrent jobs on ${node.name}`,
        value: String(node.settings.maxConcurrency),
        validateInput: (s) =>
          /^\d+$/.test(s) && +s >= 1 && +s <= 256
            ? undefined
            : 'Enter a whole number from 1 to 256',
      });
      if (v) await updateNode(node, { maxConcurrency: Number(v) });
    }),
    command('gridstator.node.setReservation', async (n?: Node) => {
      const node = nodeArg(n);
      const cpus = await vscode.window.showInputBox({
        prompt: `CPUs on ${node.name} reserved for the host (never used by jobs)`,
        value: String(node.settings.reserved.cpus),
        validateInput: (s) => (/^\d+(\.\d+)?$/.test(s) ? undefined : 'Enter a number'),
      });
      if (cpus === undefined) return;
      const gb = await vscode.window.showInputBox({
        prompt: `Memory on ${node.name} reserved for the host, in GiB`,
        value: String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10),
        validateInput: (s) => (/^\d+(\.\d+)?$/.test(s) ? undefined : 'Enter a number'),
      });
      if (gb === undefined) return;
      await updateNode(node, {
        reserved: { cpus: Number(cpus), memoryMb: Math.round(Number(gb) * 1024) },
      });
    }),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (settingUp) return;
      if (
        e.affectsConfiguration('gridstator.configPath') ||
        e.affectsConfiguration('gridstator.jobHistory') ||
        e.affectsConfiguration('gridstator.nativeCli')
      ) {
        if (e.affectsConfiguration('gridstator.configPath')) native.reset();
        if (e.affectsConfiguration('gridstator.nativeCli')) {
          native.reset();
          void launcher.refresh();
        }
        local.reset();
        watchCredentials();
        void state.connect();
      }
      if (
        e.affectsConfiguration('gridstator.autoConnect') &&
        config<boolean>('autoConnect', true)
      ) {
        local.reset();
        void state.connect();
      }
      if (e.affectsConfiguration('gridstator.defaultNode')) renderStatus();
    }),
  );

  void vscode.commands.executeCommand('setContext', 'gridstator.connection', 'connecting');
  void launcher.refresh();
  if (config<boolean>('autoConnect', true)) await state.connect();
  return { state, launcher, results, dashboard };
}

export function deactivate(): void {
  // Disposables registered on the context clean everything up.
}

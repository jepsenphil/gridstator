import { execFile } from 'node:child_process';
import { basename, relative, sep } from 'node:path';
import { promisify } from 'node:util';
import * as vscode from 'vscode';
import {
  GridstatorError,
  defaultCacheDir,
  formatBytes,
  parseBytes,
  parseDuration,
  parseMegabytes,
} from '@gridstator/shared';
import {
  DEFAULT_IMAGE,
  DEFAULT_RESOURCES,
  type Job,
  type JobGroup,
  type JobSpec,
  type JobSpecInput,
  type ProjectConfig,
} from '@gridstator/protocol';
import {
  discoverDockerfiles,
  discoverPackageScripts,
  discoverWorkflows,
  scriptCommand,
  type PackageManager,
} from '@gridstator/discovery';
import {
  captureSnapshot,
  findProject,
  loadProjectConfig,
  uploadSnapshot,
} from '@gridstator/snapshot';
import type { LaunchItem, LaunchOptions, LaunchRequest } from './messages';
import type { ClusterState } from './state';

const execFileAsync = promisify(execFile);

interface Project {
  folder: vscode.WorkspaceFolder;
  root: string;
  workdir: string;
  config: ProjectConfig;
}

/** Information needed to map a job's remote paths back to local files and to rerun it. */
export interface JobOrigin {
  root: string;
  workdir: string;
  folder: string;
  request: LaunchRequest;
}

/** A single job, or a job group when the launch was sharded. */
export type LaunchResult = Job | JobGroup;

export function isGroup(r: LaunchResult): r is JobGroup {
  return 'shardTotal' in r && 'jobs' in r;
}

export class CancelledByUser extends Error {
  constructor() {
    super('Cancelled');
  }
}

export class Launcher implements vscode.Disposable {
  items: LaunchItem[] = [];
  loading = false;
  private projects = new Map<string, Project>();
  readonly origins = new Map<string, JobOrigin>();
  readonly groupOrigins = new Map<string, JobOrigin>();
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changed.event;
  private readonly watchers: vscode.Disposable[] = [];
  private refreshTimer: NodeJS.Timeout | undefined;

  constructor(
    private readonly state: ClusterState,
    private readonly log: vscode.OutputChannel,
  ) {
    for (const pattern of [
      '**/gridstator.{yml,yaml}',
      '**/.github/workflows/*.{yml,yaml}',
      '**/package.json',
      '**/{Dockerfile,Dockerfile.*,*.Dockerfile,*.dockerfile}',
    ]) {
      const w = vscode.workspace.createFileSystemWatcher(pattern);
      const kick = () => this.scheduleRefresh();
      this.watchers.push(w, w.onDidChange(kick), w.onDidCreate(kick), w.onDidDelete(kick));
    }
    this.watchers.push(vscode.workspace.onDidChangeWorkspaceFolders(() => this.scheduleRefresh()));
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => void this.refresh(), 500);
  }

  /** Rediscovers tasks, builds, workflows, scripts and Dockerfiles in every workspace folder. */
  async refresh(): Promise<void> {
    this.loading = true;
    this.changed.fire();
    const items: LaunchItem[] = [];
    const projects = new Map<string, Project>();
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      if (folder.uri.scheme !== 'file') continue;
      try {
        const loc = await findProject(folder.uri.fsPath);
        const config = await loadProjectConfig(loc.configPath);
        projects.set(folder.name, { folder, root: loc.root, workdir: loc.workdir, config });
        items.push(...(await discoverItems(folder.name, loc.root, config)));
      } catch (err) {
        this.log.appendLine(`Discovery failed in ${folder.uri.fsPath}: ${(err as Error).message}`);
        items.push({
          id: `error:${folder.name}`,
          kind: 'command',
          label: `gridstator.yml error in ${folder.name}`,
          folder: folder.name,
          warnings: [(err as Error).message],
        });
      }
    }
    this.projects = projects;
    this.items = items;
    this.loading = false;
    this.changed.fire();
  }

  item(id: string): LaunchItem | undefined {
    return this.items.find((i) => i.id === id);
  }

  defaultNode(): string {
    return vscode.workspace.getConfiguration('gridstator').get<string>('defaultNode', '');
  }

  private project(folder: string): Project {
    const p = this.projects.get(folder) ?? [...this.projects.values()][0];
    if (!p) throw new GridstatorError('BAD_REQUEST', 'Open a folder to run jobs from it');
    return p;
  }

  /** Builds, snapshots and submits a job for a launch request. */
  async launch(request: LaunchRequest): Promise<LaunchResult> {
    const client = this.state.requireClient();
    let item = this.item(request.itemId);
    if (!item && request.itemId.startsWith('command:')) {
      item = {
        id: request.itemId,
        kind: 'command',
        label: 'command',
        folder: request.itemId.slice('command:'.length),
        warnings: [],
      };
    }
    if (!item)
      throw new GridstatorError(
        'NOT_FOUND',
        `Nothing to run for ${request.itemId}; refresh the launcher`,
      );
    const project = this.project(item.folder);
    await this.handleUnsavedFiles(project.root);
    const git = item.kind === 'workflow' ? await gitContext(project.root) : undefined;

    return vscode.window.withProgress(
      { location: vscode.ProgressLocation.Window, title: 'Gridstator' },
      async (progress) => {
        progress.report({ message: 'capturing snapshot…' });
        const captured = await captureSnapshot({
          root: project.root,
          include: project.config.snapshot.include,
          exclude: project.config.snapshot.exclude,
          allowSecrets: project.config.snapshot.allowSecrets,
          maxFileSize:
            typeof project.config.snapshot.maxFileSize === 'number'
              ? project.config.snapshot.maxFileSize * 1024 * 1024
              : parseBytes(project.config.snapshot.maxFileSize),
          cacheDir: defaultCacheDir(),
        });
        this.log.appendLine(
          `Snapshot ${captured.id.slice(7, 19)} of ${project.root}: ${captured.stats.files} files, ${formatBytes(captured.stats.totalBytes)}`,
        );
        for (const w of captured.warnings) this.log.appendLine(`  ! ${w.path}: ${w.message}`);
        progress.report({ message: 'uploading snapshot…' });
        await uploadSnapshot(captured, client);
        const spec = buildSpec(item, request.options, project, captured.id, git);
        progress.report({ message: 'submitting…' });
        const origin: JobOrigin = {
          root: project.root,
          workdir: project.workdir,
          folder: project.folder.name,
          request,
        };
        const taskShards =
          item.kind === 'task' ? project.config.tasks[item.task!.name]?.shards : undefined;
        const shards = request.options.shards ?? taskShards ?? 1;
        if (shards > 1 && spec.kind === 'command') {
          const group = await client.submitGroup({
            spec,
            shards,
            ...(spec.name ? { name: spec.name } : {}),
          });
          this.state.myGroups.add(group.id);
          this.groupOrigins.set(group.id, origin);
          for (const j of group.jobs) {
            this.state.myJobs.add(j.id);
            this.origins.set(j.id, origin);
            this.state.upsertJob(j);
          }
          this.state.upsertGroup(group);
          return group;
        }
        const job = await client.submitJob(spec);
        this.state.myJobs.add(job.id);
        this.origins.set(job.id, origin);
        this.state.upsertJob(job);
        return job;
      },
    );
  }

  /** Runs a finished job's launch again against the current files (new snapshot). */
  async rerun(jobId: string): Promise<LaunchResult> {
    const origin = this.origins.get(jobId);
    if (origin) return this.launch(origin.request);
    // Jobs started elsewhere (CLI, another window): resubmit the same spec with a fresh snapshot.
    const job = this.state.jobs.get(jobId) ?? (await this.state.requireClient().getJob(jobId));
    if (Object.values(job.spec.secretEnv).length > 0) {
      throw new GridstatorError(
        'CONFLICT',
        'This job used secrets; use Retry (same snapshot) or start it again from the Run launcher',
      );
    }
    const project = this.project(vscode.workspace.workspaceFolders?.[0]?.name ?? '');
    const client = this.state.requireClient();
    await this.handleUnsavedFiles(project.root);
    const captured = await captureSnapshot({ root: project.root, cacheDir: defaultCacheDir() });
    await uploadSnapshot(captured, client);
    const next = await client.submitJob({ ...job.spec, snapshot: captured.id });
    this.state.myJobs.add(next.id);
    this.state.upsertJob(next);
    return next;
  }

  private async handleUnsavedFiles(root: string): Promise<void> {
    const dirty = vscode.workspace.textDocuments.filter(
      (d) => d.isDirty && d.uri.scheme === 'file' && !relative(root, d.uri.fsPath).startsWith('..'),
    );
    if (dirty.length === 0) return;
    const mode = vscode.workspace
      .getConfiguration('gridstator')
      .get<string>('unsavedFiles', 'prompt');
    if (mode === 'ignore') return;
    if (mode === 'saveAll') {
      await vscode.workspace.saveAll(false);
      return;
    }
    const names =
      dirty
        .slice(0, 3)
        .map((d) => basename(d.uri.fsPath))
        .join(', ') + (dirty.length > 3 ? ', …' : '');
    const choice = await vscode.window.showWarningMessage(
      `${dirty.length} file(s) have unsaved changes (${names}). Gridstator runs the files saved on disk.`,
      { modal: true },
      'Save All and Run',
      'Run Saved Versions',
    );
    if (choice === 'Save All and Run') await vscode.workspace.saveAll(false);
    else if (choice !== 'Run Saved Versions') throw new CancelledByUser();
  }

  dispose(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    for (const w of this.watchers) w.dispose();
    this.changed.dispose();
  }
}

async function discoverItems(
  folder: string,
  root: string,
  config: ProjectConfig,
): Promise<LaunchItem[]> {
  const items: LaunchItem[] = [];
  for (const [name, task] of Object.entries(config.tasks)) {
    const command = typeof task.command === 'string' ? task.command : task.command.join(' ');
    items.push({
      id: `task:${folder}:${name}`,
      kind: 'task',
      label: name,
      ...(task.description ? { description: task.description } : {}),
      folder,
      warnings: [],
      task: {
        name,
        command,
        ...(task.junit ? { junit: task.junit } : {}),
        ...(task.shards ? { shards: task.shards } : {}),
      },
    });
  }
  for (const [name, b] of Object.entries(config.builds)) {
    items.push({
      id: `build:${folder}:${name}`,
      kind: 'build',
      label: name,
      description: b.description ?? `${b.file ?? 'Dockerfile'}${b.stage ? ` → ${b.stage}` : ''}`,
      folder,
      warnings: [],
      build: {
        name,
        ...(b.stage ? { stage: b.stage } : {}),
        tags: b.tags ?? [],
        output: b.output ?? 'load',
      },
    });
  }
  for (const w of await discoverWorkflows(root)) {
    items.push({
      id: `workflow:${folder}:${w.file}`,
      kind: 'workflow',
      label: w.name,
      description: w.file.split('/').pop() ?? w.file,
      folder,
      warnings: w.parseError ? [`Cannot parse: ${w.parseError}`] : w.warnings,
      workflow: {
        file: w.file,
        events: w.events,
        defaultEvent: w.defaultEvent,
        inputs: w.dispatchInputs,
        jobs: w.jobs.map((j) => ({
          id: j.id,
          name: j.name,
          needs: j.needs,
          runnable: j.runnable,
          warnings: j.warnings,
        })),
      },
    });
  }
  for (const pkg of await discoverPackageScripts(root)) {
    for (const [script, cmd] of Object.entries(pkg.scripts)) {
      items.push({
        id: `script:${folder}:${pkg.dir}:${script}`,
        kind: 'script',
        label: script,
        description: `${pkg.dir === '.' ? '' : `${pkg.dir} · `}${cmd}`,
        folder,
        warnings: [],
        script: {
          dir: pkg.dir,
          packageName: pkg.name,
          script,
          command: scriptCommand(pkg.packageManager, script, pkg.dir),
          packageManager: pkg.packageManager,
        },
      });
    }
  }
  for (const d of await discoverDockerfiles(root)) {
    items.push({
      id: `dockerfile:${folder}:${d.file}`,
      kind: 'dockerfile',
      label: d.file,
      description: d.stages.length ? `stages: ${d.stages.join(', ')}` : 'single stage',
      folder,
      warnings: [],
      dockerfile: { file: d.file, context: d.context, stages: d.stages },
    });
  }
  return items;
}

async function gitContext(
  root: string,
): Promise<{ branch?: string; remoteUrl?: string; headSha?: string }> {
  const run = async (args: string[]) => {
    try {
      return (await execFileAsync('git', ['-C', root, ...args])).stdout.trim() || undefined;
    } catch {
      return undefined;
    }
  };
  const [branch, remoteUrl, headSha] = await Promise.all([
    run(['rev-parse', '--abbrev-ref', 'HEAD']),
    run(['remote', 'get-url', 'origin']),
    run(['rev-parse', 'HEAD']),
  ]);
  return {
    ...(branch && branch !== 'HEAD' ? { branch } : {}),
    ...(remoteUrl ? { remoteUrl } : {}),
    ...(headSha ? { headSha } : {}),
  };
}

function secretsFrom(names: readonly string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const name of names) {
    const v = process.env[name];
    if (v === undefined) {
      throw new GridstatorError(
        'BAD_REQUEST',
        `Secret ${name} is not set in VS Code's environment`,
        {
          hint: 'Set it in the environment VS Code (or the Remote SSH server) starts with, or run this job from the CLI.',
        },
      );
    }
    out[name] = v;
  }
  return out;
}

function rel(p: string): string {
  const n = p.split(sep).join('/').replace(/^\.\//, '').replace(/\/$/, '');
  return n === '' ? '.' : n;
}

/** Turns a launch item plus user options into a job spec. */
export function buildSpec(
  item: LaunchItem,
  options: LaunchOptions,
  project: Pick<Project, 'config' | 'workdir' | 'root'>,
  snapshot: string,
  git?: { branch?: string; remoteUrl?: string; headSha?: string },
): JobSpecInput {
  const config = project.config;
  const defaults = config.defaults.resources ?? {};
  const memoryMb =
    options.memoryGb !== undefined
      ? Math.round(options.memoryGb * 1024)
      : defaults.memory === undefined
        ? DEFAULT_RESOURCES.memoryMb
        : typeof defaults.memory === 'number'
          ? defaults.memory
          : parseMegabytes(defaults.memory);
  const node =
    options.node ?? vscode.workspace.getConfiguration('gridstator').get<string>('defaultNode', '');
  const common = {
    snapshot,
    resources: { cpus: options.cpus ?? defaults.cpus ?? DEFAULT_RESOURCES.cpus, memoryMb },
    target: {
      labels: config.defaults.labels ?? [],
      ...(node ? { node } : {}),
      prefer: [...(config.defaults.prefer ?? []), ...(options.prefer ?? [])],
    },
    ...(config.defaults.timeout
      ? { timeoutSeconds: Math.ceil(parseDuration(config.defaults.timeout) / 1000) }
      : {}),
    project: config.project ?? basename(project.root),
    env: options.env ?? {},
  };
  switch (item.kind) {
    case 'task': {
      const task = config.tasks[item.task!.name];
      if (!task) throw new GridstatorError('NOT_FOUND', `Task ${item.task!.name} no longer exists`);
      const command = typeof task.command === 'string' ? ['sh', '-c', task.command] : task.command;
      const artifacts = [...(task.artifacts ?? []), ...(task.junit ? [task.junit] : [])].map(rel);
      return {
        kind: 'command',
        ...common,
        name: `task:${item.task!.name}`,
        image: options.image ?? task.image ?? config.defaults.image ?? DEFAULT_IMAGE,
        command,
        env: { ...task.env, ...common.env },
        secretEnv: secretsFrom(task.secretEnv ?? []),
        workdir: task.workdir ? rel(task.workdir) : '.',
        artifacts: [...new Set(artifacts)],
        ...(task.timeout ? { timeoutSeconds: Math.ceil(parseDuration(task.timeout) / 1000) } : {}),
        ...(task.resources?.cpus !== undefined && options.cpus === undefined
          ? { resources: { ...common.resources, cpus: task.resources.cpus } }
          : {}),
        target: {
          labels: [...common.target.labels, ...(task.labels ?? [])],
          ...(node || task.node ? { node: node || task.node } : {}),
        },
      };
    }
    case 'script':
      return {
        kind: 'command',
        ...common,
        name: `${item.script!.dir === '.' ? '' : `${item.script!.dir}: `}${item.script!.packageManager} run ${item.script!.script}`,
        image: options.image ?? config.defaults.image ?? DEFAULT_IMAGE,
        command: ['sh', '-c', item.script!.command],
        workdir: '.',
      };
    case 'command': {
      const command = options.command?.trim();
      if (!command) throw new GridstatorError('BAD_REQUEST', 'Enter a command to run');
      return {
        kind: 'command',
        ...common,
        image: options.image ?? config.defaults.image ?? DEFAULT_IMAGE,
        command: ['sh', '-c', command],
        workdir: project.workdir,
      };
    }
    case 'workflow': {
      const wf = item.workflow!;
      return {
        kind: 'actions',
        ...common,
        workflow: wf.file,
        ...(options.job ? { job: options.job } : {}),
        event: options.event ?? wf.defaultEvent,
        inputs: options.inputs ?? {},
        vars: config.actions.vars ?? {},
        platforms: config.actions.platforms ?? {},
        dockerSocket: config.actions.dockerSocket ?? false,
        secretEnv: secretsFrom(config.actions.secretEnv ?? []),
        ...(git ? { git } : {}),
      };
    }
    case 'dockerfile': {
      const d = item.dockerfile!;
      return {
        kind: 'docker-build',
        ...common,
        name: `build ${d.file}${options.stage ? ` --target ${options.stage}` : ''}`,
        context: d.context,
        dockerfile: d.file,
        ...(options.stage ? { stage: options.stage } : {}),
        tags: options.tags ?? [],
        output: options.output ?? 'load',
        buildArgs: options.buildArgs ?? {},
      };
    }
    case 'build': {
      const b = config.builds[item.build!.name];
      if (!b) throw new GridstatorError('NOT_FOUND', `Build ${item.build!.name} no longer exists`);
      const stage = options.stage ?? b.stage;
      return {
        kind: 'docker-build',
        ...common,
        name: `build:${item.build!.name}`,
        context: b.context ? rel(b.context) : '.',
        ...(b.file ? { dockerfile: rel(b.file) } : {}),
        ...(stage ? { stage } : {}),
        tags: options.tags ?? b.tags ?? [],
        output: options.output ?? b.output ?? 'load',
        buildArgs: { ...b.buildArgs, ...options.buildArgs },
        platforms: b.platforms ?? [],
        secretEnv: secretsFrom(b.secretEnv ?? []),
      };
    }
  }
}

/** Short description of what a job does, for notifications and trees. */
export function describeSpec(spec: JobSpec): string {
  switch (spec.kind) {
    case 'command':
      return spec.command.length >= 3 && spec.command[0] === 'sh'
        ? (spec.command[2] ?? '')
        : spec.command.join(' ');
    case 'docker-build':
      return `docker build${spec.stage ? ` --target ${spec.stage}` : ''}`;
    case 'actions':
      return `act ${spec.event} ${spec.workflow}${spec.job ? ` -j ${spec.job}` : ''}`;
  }
}

export type { PackageManager };

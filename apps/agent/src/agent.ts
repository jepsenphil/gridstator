import { mkdirSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import type { WebSocket } from 'ws';
import {
  GRIDSTATOR_VERSION,
  GridstatorError,
  decodeWsData,
  detectBuild,
  errorMessage,
  silentLogger,
  writeFileAtomic,
  type BuildIdentity,
  type Logger,
} from '@gridstator/shared';
import {
  AGENT_PROTOCOL_VERSION,
  AgentJobResultSchema,
  ControllerMessageSchema,
  type AgentJobResult,
  type AgentMessage,
  type ControllerJobAssign,
  type ControllerMessage,
  type LogEntry,
  type NodeInfo,
  type NodeSettings,
  type NodeSettingsPatch,
} from '@gridstator/protocol';
import { GridstatorClient } from '@gridstator/client';
import {
  ActionsExecutor,
  ContainerExecutor,
  DockerBuildExecutor,
  DockerCli,
  cleanupManagedResources,
  type Executor,
} from '@gridstator/executors';
import { BlobStore } from '@gridstator/snapshot';
import { readSpool, removeSpool } from './log-pipeline.js';
import { JobRunner } from './runner.js';
import { updateAgentState, type AgentState } from './state.js';
import { MetricsSampler, actBinary, collectNodeInfo } from './system.js';
import { AgentToolchain } from './toolchain.js';
import { Updater } from './updater.js';
import { relocateController } from './relocation.js';

export interface AgentOptions {
  dataDir: string;
  state: AgentState;
  /** Settings explicitly given on the command line; they override the controller's values. */
  settings?: NodeSettingsPatch;
  logger?: Logger;
  docker?: DockerCli;
  heartbeatIntervalMs?: number;
  /** Local blob cache size limit. Default 10 GiB. */
  cacheMaxBytes?: number;
  maxLogBytesPerJob?: number;
  keepFailedWorkspaces?: boolean;
  /** The running build; detected when omitted. */
  build?: BuildIdentity;
  /**
   * Let the controller update this agent. Only for `agent start` from a managed install: an agent
   * embedded in the controller process must never touch the controller's version store.
   */
  selfUpdate?: boolean;
}

/** `restarting`: a new version is installed; stop and exit with RESTART_EXIT_CODE. */
export type AgentStatus = 'connecting' | 'connected' | 'disconnected' | 'stopped' | 'restarting';

/**
 * Worker agent: keeps one outbound WebSocket to the controller, runs assigned jobs, and buffers
 * results and logs across disconnects (ARCHITECTURE §2.3, §8).
 */
export class Agent {
  private readonly opts: AgentOptions;
  private readonly log: Logger;
  private readonly docker: DockerCli;
  private readonly client: GridstatorClient;
  private readonly cache: BlobStore;
  private sampler: MetricsSampler;
  private readonly dirs: { logs: string; results: string; tmp: string; work: string };
  private readonly runners = new Map<string, JobRunner>();
  private readonly results = new Map<string, AgentJobResult>();
  private executors = new Map<string, Executor>();
  private info: NodeInfo | undefined;
  private ws: WebSocket | undefined;
  private welcomed = false;
  private stopping = false;
  private fatal: string | undefined;
  /** The controller speaks another agent protocol; only the update endpoint is used. */
  private updateOnly = false;
  private updater: Updater | undefined;
  private toolchain: AgentToolchain | undefined;
  private updating = false;
  private reconnectAttempt = 0;
  private relocating = false;
  private desiredControllerUrl: string | undefined;
  private reconnectTimer: NodeJS.Timeout | undefined;
  private heartbeatTimer: NodeJS.Timeout | undefined;
  private statusListeners = new Set<(s: AgentStatus, detail?: string) => void>();
  private incomingMessages: Promise<void> = Promise.resolve();
  settings: NodeSettings | undefined;

  constructor(opts: AgentOptions) {
    this.opts = opts;
    this.log = opts.logger ?? silentLogger();
    this.docker = opts.docker ?? new DockerCli();
    this.client = new GridstatorClient({
      url: opts.state.controllerUrl,
      ca: opts.state.caPem,
      token: opts.state.credential,
    });
    this.cache = new BlobStore(join(opts.dataDir, 'cas'));
    this.dirs = {
      logs: join(opts.dataDir, 'logs'),
      results: join(opts.dataDir, 'results'),
      tmp: join(opts.dataDir, 'tmp'),
      work: join(opts.dataDir, 'work'),
    };
    this.sampler = new MetricsSampler([opts.dataDir]);
  }

  get nodeId(): string {
    return this.opts.state.nodeId;
  }

  get activeJobIds(): string[] {
    return [...this.runners.keys()];
  }

  onStatus(listener: (s: AgentStatus, detail?: string) => void): () => void {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  private setStatus(s: AgentStatus, detail?: string): void {
    for (const l of this.statusListeners) l(s, detail);
  }

  async start(): Promise<void> {
    for (const d of Object.values(this.dirs)) mkdirSync(d, { recursive: true, mode: 0o700 });
    // Temporary files and work directories of a previous agent process are never reused.
    rmSync(this.dirs.tmp, { recursive: true, force: true });
    rmSync(this.dirs.work, { recursive: true, force: true });
    mkdirSync(this.dirs.work, { recursive: true, mode: 0o700 });
    mkdirSync(this.dirs.tmp, { recursive: true, mode: 0o700 });
    await this.cache.init();
    const build = this.opts.build ?? (await detectBuild(GRIDSTATOR_VERSION));
    this.updater = new Updater({
      client: this.client,
      build,
      selfUpdate: this.opts.selfUpdate ?? false,
      tmpDir: this.dirs.tmp,
      logger: this.log,
      restart: () => this.setStatus('restarting'),
    });
    this.toolchain = new AgentToolchain({
      dataDir: this.opts.dataDir,
      client: this.client,
      logger: this.log,
      installRoot: this.updater.managed ? (build.install?.root ?? null) : null,
    });
    await this.toolchain.load();
    // An explicit GRIDSTATOR_ACT wins; then the managed act; then act from PATH.
    const managedAct = process.env['GRIDSTATOR_ACT'] ? undefined : this.toolchain.actPath();
    const actBin = managedAct ?? actBinary();
    const info = await collectNodeInfo(this.docker, actBin);
    this.info = {
      ...info,
      controllerUrl: this.client.url,
      build: await this.updater.describe(this.toolchain.tools(info.act, managedAct !== undefined)),
    };
    await this.updater.startWatchdog();
    if (this.info.docker.available) {
      const root = await this.docker
        .exec(['info', '--format', '{{.DockerRootDir}}'])
        .catch(() => undefined);
      const dockerRoot = root?.code === 0 ? root.stdout.trim() : '';
      if (dockerRoot) this.sampler = new MetricsSampler([dockerRoot, this.opts.dataDir]);
      this.executors.set(
        'command',
        new ContainerExecutor({
          docker: this.docker,
          tmpDir: this.dirs.tmp,
          keepFailed: this.opts.keepFailedWorkspaces ?? false,
        }),
      );
      if (this.info.executors.includes('docker-build')) {
        this.executors.set('docker-build', new DockerBuildExecutor({ docker: this.docker }));
      }
      if (this.info.executors.includes('actions')) {
        this.executors.set(
          'actions',
          new ActionsExecutor({
            docker: this.docker,
            actBin,
            actHome: join(this.opts.dataDir, 'act-home'),
            tmpDir: this.dirs.tmp,
          }),
        );
      }
      // Anything labelled for this node is left over from a previous agent process.
      const removed = await cleanupManagedResources(this.docker, this.nodeId, new Set());
      if (removed.containers.length + removed.volumes.length > 0) {
        this.log.warn(removed, 'removed job containers/volumes left by a previous agent process');
      }
    } else {
      this.log.warn(
        { error: this.info.docker.error },
        'Docker is not available; this node cannot run jobs',
      );
    }
    this.loadResults();
    this.log.info(
      {
        node: this.opts.state.name,
        controller: this.opts.state.controllerUrl,
        docker: this.info.docker.version ?? 'unavailable',
      },
      'agent starting',
    );
    this.connect();
    this.heartbeatTimer = setInterval(
      () => void this.heartbeat(),
      this.opts.heartbeatIntervalMs ?? 5000,
    );
  }

  private loadResults(): void {
    for (const file of readdirSync(this.dirs.results)) {
      if (!file.endsWith('.json')) continue;
      try {
        const parsed = AgentJobResultSchema.parse(
          JSON.parse(readFileSync(join(this.dirs.results, file), 'utf8')),
        );
        this.results.set(parsed.jobId, parsed);
      } catch {
        rmSync(join(this.dirs.results, file), { force: true });
      }
    }
    // Spools without a pending result belong to jobs that were killed with the previous process.
    for (const file of readdirSync(this.dirs.logs)) {
      const jobId = file.replace(/\.ndjson$/, '');
      if (!this.results.has(jobId)) rmSync(join(this.dirs.logs, file), { force: true });
    }
  }

  // ---- connection ----------------------------------------------------------------------------

  private connect(): void {
    if (this.stopping || this.fatal || this.updateOnly) return;
    this.setStatus('connecting');
    const ws = this.client.openWebSocket('/v1/agent/ws');
    this.ws = ws;
    this.welcomed = false;
    ws.on('open', () => {
      this.reconnectAttempt = 0;
      this.send({
        type: 'hello',
        protocolVersion: AGENT_PROTOCOL_VERSION,
        info: this.info!,
        ...(this.opts.settings ? { settings: this.opts.settings } : {}),
        activeJobs: [...this.runners.values()].map((r) => ({
          jobId: r.jobId,
          assignmentId: r.assignmentId,
          phase: r.phase,
          logSeq: r.logSeq,
        })),
        results: [...this.results.values()],
      });
    });
    ws.on('message', (data) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(decodeWsData(data));
      } catch {
        return;
      }
      const msg = ControllerMessageSchema.safeParse(parsed);
      if (!msg.success) {
        this.log.warn({ issues: msg.error.issues.slice(0, 3) }, 'invalid message from controller');
        return;
      }
      this.incomingMessages = this.incomingMessages
        .then(() => this.handle(msg.data, ws))
        .catch((err: unknown) => {
          this.log.error({ err, type: msg.data.type }, 'failed to handle controller message');
        });
    });
    ws.on('unexpected-response', (_req, res) => {
      const status = res.statusCode ?? 0;
      res.resume();
      if (status === 401 || status === 403) {
        this.fatal = `The controller rejected this agent's credential (HTTP ${status}). The node may have been removed; re-join with \`gridstator agent join --force\`.`;
        this.log.error(this.fatal);
        this.setStatus('stopped', this.fatal);
      } else {
        this.log.warn({ status }, 'controller refused the connection');
      }
      ws.terminate();
    });
    ws.on('error', (err) => {
      this.log.debug({ err: errorMessage(err) }, 'controller connection error');
    });
    ws.on('close', (code, reason) => {
      if (this.ws === ws) this.ws = undefined;
      this.welcomed = false;
      if (this.stopping || this.fatal || this.updateOnly) return;
      const delay =
        Math.min(30_000, 1000 * 2 ** Math.min(this.reconnectAttempt, 5)) *
        (0.75 + Math.random() * 0.5);
      this.reconnectAttempt++;
      this.log.warn(
        { code, reason: reason.toString(), retryInMs: Math.round(delay) },
        'disconnected from controller',
      );
      this.setStatus('disconnected', `retrying in ${Math.round(delay / 1000)}s`);
      this.reconnectTimer = setTimeout(() => void this.reconnect(), delay);
    });
  }

  private async reconnect(): Promise<void> {
    if (this.stopping || this.fatal || this.updateOnly) return;
    if (this.reconnectAttempt >= 2) {
      for (const url of this.opts.state.controllerFallbackUrls ?? []) {
        try {
          if (await relocateController(this.opts.dataDir, this.opts.state, this.client, url)) {
            if (this.info) this.info.controllerUrl = this.client.url;
            break;
          }
        } catch {
          /* An unavailable fallback never changes the pinned connection. */
        }
      }
    }
    this.connect();
  }

  private async moveController(url: string): Promise<void> {
    if (this.relocating || this.stopping || url === this.client.url) return;
    this.relocating = true;
    try {
      if (await relocateController(this.opts.dataDir, this.opts.state, this.client, url)) {
        if (this.info) this.info.controllerUrl = this.client.url;
        this.log.info({ controller: this.client.url }, 'controller address verified and saved');
        this.ws?.close(1000, 'reconnecting at the new controller address');
      }
    } catch (err) {
      this.log.debug(
        { error: errorMessage(err) },
        'new controller address is not reachable yet; keeping current connection',
      );
    } finally {
      this.relocating = false;
    }
  }

  private send(msg: AgentMessage): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== 1) return false;
    ws.send(JSON.stringify(msg));
    return true;
  }

  private currentConnection(socket: WebSocket): boolean {
    return this.ws === socket && socket.readyState === 1 && !this.stopping;
  }

  private async handle(msg: ControllerMessage, socket: WebSocket): Promise<void> {
    if (!this.currentConnection(socket)) return;
    switch (msg.type) {
      case 'welcome':
        await this.adoptName(msg.nodeId, msg.name);
        if (!this.currentConnection(socket)) return;
        this.onWelcome(msg.settings, msg.resume);
        this.desiredControllerUrl = msg.controllerUrl;
        if (msg.controllerUrl) void this.moveController(msg.controllerUrl);
        return;
      case 'job.assign':
        return this.onAssign(msg);
      case 'job.cancel':
        this.runners.get(msg.jobId)?.cancel(msg.reason);
        return;
      case 'job.ack':
        return this.onAck(msg.jobId);
      case 'node.settings':
        await this.adoptName(this.nodeId, msg.name);
        if (!this.currentConnection(socket)) return;
        this.settings = msg.settings;
        this.log.info({ settings: msg.settings }, 'node settings changed');
        return;
      case 'update.check':
        void this.onUpdateCheck();
        return;
      case 'error':
        this.log.error({ code: msg.code }, msg.message);
        if (msg.code === 'PROTOCOL_MISMATCH' && this.updater?.startUpdateOnly()) {
          this.updateOnly = true;
          this.setStatus('disconnected', 'waiting for an update from the controller');
          this.ws?.close();
          return;
        }
        if (msg.fatal) {
          this.fatal = msg.message;
          this.setStatus('stopped', msg.message);
          this.ws?.close();
        }
        return;
    }
  }

  private async adoptName(nodeId: string, name: string | undefined): Promise<void> {
    if (nodeId !== this.nodeId)
      throw new GridstatorError('FORBIDDEN', 'The controller returned another worker identity');
    if (name === undefined || name === this.opts.state.name) return;
    await updateAgentState(this.opts.dataDir, this.opts.state, (current) => ({ ...current, name }));
  }

  /** Updates the build, then (on a current build) the toolchain; restarts once if needed. */
  private async onUpdateCheck(): Promise<void> {
    const updater = this.updater;
    if (!updater || this.updating || this.stopping) return;
    this.updating = true;
    try {
      const outcome = await updater.check();
      let restart = outcome === 'restart';
      if (outcome === 'none' && updater.managed && this.toolchain && this.info) {
        try {
          restart = await this.toolchain.sync(this.info.platform, this.info.arch);
        } catch (err) {
          const message = `toolchain: ${errorMessage(err)}`;
          this.log.error({ err: message }, 'toolchain update failed');
          await this.client
            .reportAgentUpdate(updater.buildId, message)
            .catch((e: unknown) =>
              this.log.warn({ err: errorMessage(e) }, 'could not report the failed update'),
            );
        }
      }
      if (restart) this.setStatus('restarting');
    } finally {
      this.updating = false;
    }
  }

  private onWelcome(
    settings: NodeSettings,
    resume: Array<{ jobId: string; logSeq: number }>,
  ): void {
    this.settings = settings;
    // Replay spooled log entries the controller has not seen, then any pending results — all
    // synchronously, so live entries cannot interleave with the replay.
    for (const { jobId, logSeq } of resume) {
      const runner = this.runners.get(jobId);
      const result = this.results.get(jobId);
      const spool = runner?.spoolPath ?? join(this.dirs.logs, `${jobId}.ndjson`);
      const assignmentId = runner?.assignmentId ?? result?.assignmentId;
      if (!assignmentId) continue;
      const entries = readSpool(spool, logSeq);
      for (let i = 0; i < entries.length; i += 2000) {
        this.send({ type: 'job.log', jobId, assignmentId, entries: entries.slice(i, i + 2000) });
      }
      if (result && !runner) this.send({ type: 'job.result', result });
    }
    this.welcomed = true;
    this.setStatus('connected');
    void this.updater?.joined();
    this.log.info(
      { node: this.opts.state.name, resumed: resume.length },
      'connected to controller',
    );
    void this.heartbeat();
  }

  private onAssign(msg: ControllerJobAssign): void {
    const existing = this.runners.get(msg.jobId);
    if (existing) {
      if (existing.assignmentId !== msg.assignmentId) {
        this.send({
          type: 'job.rejected',
          jobId: msg.jobId,
          assignmentId: msg.assignmentId,
          reason: 'a previous attempt of this job is still stopping',
        });
      }
      return;
    }
    if (this.stopping) {
      this.send({
        type: 'job.rejected',
        jobId: msg.jobId,
        assignmentId: msg.assignmentId,
        reason: 'agent is shutting down',
      });
      return;
    }
    const executor = this.executors.get(msg.spec.kind);
    if (!executor) {
      const reason = this.info?.docker.available
        ? `this agent cannot run ${msg.spec.kind} jobs`
        : `Docker is unavailable (${this.info?.docker.error ?? 'unknown'})`;
      this.send({ type: 'job.rejected', jobId: msg.jobId, assignmentId: msg.assignmentId, reason });
      return;
    }
    this.results.delete(msg.jobId);
    rmSync(join(this.dirs.results, `${msg.jobId}.json`), { force: true });
    removeSpool(join(this.dirs.logs, `${msg.jobId}.ndjson`));
    const runner = new JobRunner(msg, {
      client: this.client,
      cache: this.cache,
      executor,
      nodeId: this.nodeId,
      nodeName: this.opts.state.name,
      logsDir: this.dirs.logs,
      workRoot: this.dirs.work,
      maxLogBytes: this.opts.maxLogBytesPerJob ?? 64 * 1024 * 1024,
      logger: this.log,
      sendLogs: (r, entries: LogEntry[]) => {
        if (this.welcomed && this.runners.get(r.jobId) === r) {
          this.send({ type: 'job.log', jobId: r.jobId, assignmentId: r.assignmentId, entries });
        }
      },
      sendOutputs: (r, outputs) => {
        if (this.welcomed && this.runners.get(r.jobId) === r) {
          this.send({ type: 'job.outputs', jobId: r.jobId, assignmentId: r.assignmentId, outputs });
        }
      },
      sendPhase: (r, phase) => {
        if (this.welcomed)
          this.send({
            type: 'job.state',
            jobId: r.jobId,
            assignmentId: r.assignmentId,
            phase,
            ts: Date.now(),
          });
      },
      complete: (r, result) => this.onComplete(r, result),
    });
    this.runners.set(msg.jobId, runner);
    this.log.info({ jobId: msg.jobId, name: msg.spec.name }, 'job accepted');
    runner.start();
  }

  private onComplete(runner: JobRunner, result: AgentJobResult): void {
    this.results.set(result.jobId, result);
    void writeFileAtomic(
      join(this.dirs.results, `${result.jobId}.json`),
      JSON.stringify(result),
    ).catch((err: unknown) => this.log.error({ err }, 'could not persist job result'));
    if (this.runners.get(runner.jobId) === runner) this.runners.delete(runner.jobId);
    if (this.welcomed) this.send({ type: 'job.result', result });
    void this.evictCache();
  }

  private onAck(jobId: string): void {
    if (this.runners.has(jobId)) return;
    this.results.delete(jobId);
    rmSync(join(this.dirs.results, `${jobId}.json`), { force: true });
    removeSpool(join(this.dirs.logs, `${jobId}.ndjson`));
  }

  private async evictCache(): Promise<void> {
    const keep = new Set<string>();
    for (const r of this.runners.values()) {
      if (!r.manifest) continue;
      for (const e of r.manifest.entries) if (e.type === 'file') keep.add(e.digest);
    }
    try {
      const freed = await this.cache.evictTo(this.opts.cacheMaxBytes ?? 10 * 1024 ** 3, keep);
      if (freed > 0) this.log.info({ freedBytes: freed }, 'evicted snapshot cache entries');
    } catch (err) {
      this.log.warn({ err }, 'cache eviction failed');
    }
  }

  private async heartbeat(): Promise<void> {
    if (!this.welcomed) return;
    if (this.desiredControllerUrl && this.desiredControllerUrl !== this.client.url) {
      void this.moveController(this.desiredControllerUrl);
    }
    try {
      const metrics = await this.sampler.sample();
      this.send({ type: 'heartbeat', metrics, activeJobs: this.activeJobIds });
    } catch (err) {
      this.log.warn({ err }, 'failed to sample metrics');
    }
  }

  /**
   * Stops the agent. By default running jobs are stopped and reported as lost; with `drain`,
   * new work is refused and running jobs are allowed to finish first.
   */
  async stop(options: { drain?: boolean; timeoutMs?: number } = {}): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    if (!options.drain) for (const r of this.runners.values()) r.abandon('the agent was shut down');
    const pending = [...this.runners.values()].map((r) => r.done);
    if (pending.length > 0) {
      this.log.info(
        { jobs: pending.length, drain: options.drain ?? false },
        'waiting for running jobs',
      );
      await Promise.race([
        Promise.all(pending),
        new Promise((r) =>
          setTimeout(r, options.timeoutMs ?? (options.drain ? 24 * 3600_000 : 60_000)),
        ),
      ]);
    }
    // Give the final results a moment to reach the controller.
    await new Promise((r) => setTimeout(r, 200));
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.updater?.stop();
    this.ws?.close(1001, 'agent shutting down');
    await this.incomingMessages;
    this.client.close();
    this.setStatus('stopped');
  }

  /** Throws if the agent stopped because of a fatal controller error. */
  assertHealthy(): void {
    if (this.fatal) throw new GridstatorError('UNAUTHORIZED', this.fatal);
  }
}

import { GridstatorError, newId, type Logger } from '@gridstator/shared';
import {
  AGENT_PROTOCOL_VERSION,
  NodeSettingsSchema,
  automaticLabels,
  groupState,
  shardSpec,
  canTransition,
  defaultNodeSettings,
  isTerminal,
  jobDisplayName,
  redactSpec,
  type AgentHello,
  type AgentJobResult,
  type AgentMessage,
  type ControllerMessage,
  type Job,
  type JobSpec,
  type JobOutputs,
  type JobGroup,
  type JobState,
  type LogEntry,
  type Node,
  type NodeInfo,
  type NodeMetrics,
  type NodeSettings,
  type NodeSettingsPatch,
  type PatchNodeRequest,
} from '@gridstator/protocol';
import {
  allocatable,
  schedule,
  type SchedulerJob,
  type SchedulerNode,
} from '@gridstator/scheduler';
import type { Auth, Principal } from './auth.js';
import type { GroupRow, JobRow, NodeRow, Store } from './db.js';
import type { EventHub } from './hub.js';
import type { LogStore } from './logs.js';
import type { UpdateHost, UpdateManager, UpdateNode } from './updates.js';

export interface ClusterTiming {
  /** An agent connection without a heartbeat for this long is closed. */
  heartbeatTimeoutMs: number;
  /** Jobs on an offline node become lost (or are requeued) after this long. */
  lostTimeoutMs: number;
  /** A cancellation the agent does not confirm is finalized after this long. */
  cancelTimeoutMs: number;
  /** An assignment the agent does not acknowledge is requeued after this long. */
  assignTimeoutMs: number;
  sweepIntervalMs: number;
  scheduleIntervalMs: number;
}

export const DEFAULT_TIMING: ClusterTiming = {
  heartbeatTimeoutMs: 20_000,
  lostTimeoutMs: 60_000,
  cancelTimeoutMs: 30_000,
  assignTimeoutMs: 30_000,
  sweepIntervalMs: 2_000,
  scheduleIntervalMs: 5_000,
};

/** The agent side of a WebSocket, abstracted for testing. */
export interface AgentConnection {
  send(msg: ControllerMessage): boolean;
  close(code: number, reason: string): void;
}

interface NodeRuntime {
  conn: AgentConnection | undefined;
  welcomed: boolean;
  metrics: NodeMetrics | null;
  connectedAt: number | null;
  lastHeartbeatAt: number | null;
  /** When the node last went offline (controller start counts as offline). */
  disconnectedAt: number;
}

const DISPATCHED: JobState[] = ['assigned', 'starting', 'running'];
const REJECTION_TTL_MS = 10 * 60_000;
const WARM_WINDOW_MS = 24 * 3600_000;

interface TransitionOptions {
  actor: string;
  message?: string | null;
  patch?: Partial<Omit<JobRow, 'id' | 'spec' | 'state'>>;
  eventType?: string;
}

export class Cluster {
  readonly startedAt = Date.now();
  private readonly store: Store;
  private readonly logs: LogStore;
  private readonly hub: EventHub;
  private readonly auth: Auth;
  private readonly log: Logger;
  private readonly timing: ClusterTiming;
  private readonly runtimes = new Map<string, NodeRuntime>();
  private readonly rejections = new Map<string, Map<string, number>>();
  private scheduleQueued = false;
  private timers: NodeJS.Timeout[] = [];
  private stopped = false;
  private readonly updates: UpdateManager | undefined;
  private readonly controllerEndpoint: (() => string | undefined) | undefined;

  constructor(deps: {
    store: Store;
    logs: LogStore;
    hub: EventHub;
    auth: Auth;
    logger: Logger;
    timing?: Partial<ClusterTiming>;
    updates?: UpdateManager;
    controllerEndpoint?: () => string | undefined;
  }) {
    this.store = deps.store;
    this.logs = deps.logs;
    this.hub = deps.hub;
    this.auth = deps.auth;
    this.log = deps.logger;
    this.timing = { ...DEFAULT_TIMING, ...deps.timing };
    this.updates = deps.updates;
    this.controllerEndpoint = deps.controllerEndpoint;
    this.updates?.attach(this.updateHost());
  }

  private updateNode(row: NodeRow, allocs = this.allocations()): UpdateNode {
    const rt = this.runtime(row.id);
    return {
      id: row.id,
      name: row.name,
      online: rt.conn !== undefined && rt.welcomed,
      build: row.info?.build,
      agentVersion: row.info?.agentVersion,
      autoUpdate: row.settings.autoUpdate,
      activeJobs: allocs.get(row.id)?.jobs.length ?? 0,
      platform: row.info?.platform,
      arch: row.info?.arch,
    };
  }

  private updateHost(): UpdateHost {
    return {
      nodes: () => {
        const allocs = this.allocations();
        return this.store.listNodes().map((n) => this.updateNode(n, allocs));
      },
      node: (idOrName) => {
        const row = this.store.nodeById(idOrName) ?? this.store.nodeByName(idOrName);
        return row ? this.updateNode(row) : undefined;
      },
      sendUpdateCheck: (nodeId) => {
        const rt = this.runtimes.get(nodeId);
        return rt?.conn && rt.welcomed ? rt.conn.send({ type: 'update.check' }) : false;
      },
      cancelJobs: (nodeId, reason) => {
        for (const job of this.store.jobsOnNode(nodeId, DISPATCHED)) {
          try {
            this.cancel(job.id, 'update');
            this.note(job, 'update', 'update', reason);
          } catch (err) {
            this.log.warn({ err, jobId: job.id }, 'could not cancel job for update');
          }
        }
      },
      changed: (nodeId) => this.emitNode(nodeId),
      requestSchedule: () => this.requestSchedule(),
    };
  }

  start(): void {
    this.timers.push(setInterval(() => this.sweep(), this.timing.sweepIntervalMs));
    this.timers.push(setInterval(() => this.requestSchedule(), this.timing.scheduleIntervalMs));
    const inFlight = this.store.jobsInStates(DISPATCHED).length;
    if (inFlight > 0) {
      this.log.info(
        { jobs: inFlight },
        'waiting for agents to reconnect and report in-flight jobs',
      );
    }
    this.requestSchedule();
  }

  stop(): void {
    this.stopped = true;
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    for (const rt of this.runtimes.values()) rt.conn?.close(1001, 'controller shutting down');
  }

  private runtime(nodeId: string): NodeRuntime {
    let rt = this.runtimes.get(nodeId);
    if (!rt) {
      rt = {
        conn: undefined,
        welcomed: false,
        metrics: null,
        connectedAt: null,
        lastHeartbeatAt: null,
        disconnectedAt: this.startedAt,
      };
      this.runtimes.set(nodeId, rt);
    }
    return rt;
  }

  // ---- views ---------------------------------------------------------------------------------

  jobView(row: JobRow): Job {
    return {
      id: row.id,
      name: row.name,
      kind: row.kind,
      state: row.state,
      stateReason: row.state_reason,
      priority: row.priority,
      owner: row.owner,
      spec: redactSpec(row.spec),
      snapshotId: row.snapshot_id,
      nodeId: row.node_id,
      nodeName: row.node_name,
      assignmentId: row.assignment_id,
      attempt: row.attempt,
      retryOf: row.retry_of,
      exitCode: row.exit_code,
      error: row.error,
      queuePosition: row.state === 'queued' ? this.store.queuePosition(row) : null,
      logSeq: row.log_seq,
      createdAt: row.created_at,
      queuedAt: row.queued_at,
      assignedAt: row.assigned_at,
      startedAt: row.started_at,
      finishedAt: row.finished_at,
      cancelRequestedAt: row.cancel_requested_at,
      outputs: row.outputs,
      groupId: row.group_id,
      shardIndex: row.shard_index,
      shardTotal: row.shard_total,
      artifacts: this.store.artifactsForJob(row.id),
    };
  }

  private allocations(): Map<string, { cpus: number; memoryMb: number; jobs: string[] }> {
    const map = new Map<string, { cpus: number; memoryMb: number; jobs: string[] }>();
    for (const job of this.store.jobsInStates(DISPATCHED)) {
      if (!job.node_id) continue;
      const a = map.get(job.node_id) ?? { cpus: 0, memoryMb: 0, jobs: [] };
      a.cpus += job.spec.resources.cpus;
      a.memoryMb += job.spec.resources.memoryMb;
      a.jobs.push(job.id);
      map.set(job.node_id, a);
    }
    return map;
  }

  nodeView(row: NodeRow, allocs = this.allocations()): Node {
    const rt = this.runtime(row.id);
    const online = rt.conn !== undefined && rt.welcomed;
    const alloc = allocs.get(row.id) ?? { cpus: 0, memoryMb: 0, jobs: [] };
    const issues: string[] = [];
    if (online && row.info) {
      if (!row.info.docker.available)
        issues.push(`Docker unavailable: ${row.info.docker.error ?? 'unknown error'}`);
      if (rt.metrics && rt.metrics.diskFreeMb < 5 * 1024)
        issues.push(`Low disk space (${Math.floor(rt.metrics.diskFreeMb / 1024)} GiB free)`);
    }
    return {
      id: row.id,
      name: row.name,
      status: online ? 'online' : 'offline',
      health: !online ? 'unknown' : issues.length > 0 ? 'degraded' : 'ok',
      healthIssues: issues,
      info: row.info,
      ...(this.updates
        ? { update: this.updates.status(this.updateNode(row, allocs), row.last_seen_at) }
        : {}),
      metrics: online ? rt.metrics : null,
      settings: row.settings,
      effectiveLabels: [...automaticLabels(row.info), ...row.settings.labels],
      activeJobs: alloc.jobs,
      allocated: { cpus: alloc.cpus, memoryMb: alloc.memoryMb },
      allocatable: allocatable({ info: row.info, settings: row.settings }),
      lastSeenAt: row.last_seen_at,
      connectedAt: online ? rt.connectedAt : null,
      createdAt: row.created_at,
    };
  }

  listNodes(): Node[] {
    const allocs = this.allocations();
    return this.store.listNodes().map((n) => this.nodeView(n, allocs));
  }

  findNode(idOrName: string): NodeRow {
    const row = this.store.nodeById(idOrName) ?? this.store.nodeByName(idOrName);
    if (!row)
      throw new GridstatorError('NOT_FOUND', `No node with id or name "${idOrName}"`, {
        hint: 'List nodes with `gridstator nodes`.',
      });
    return row;
  }

  getJob(id: string): JobRow {
    const row = this.store.jobById(id);
    if (!row) throw new GridstatorError('NOT_FOUND', `Job ${id} not found`);
    return row;
  }

  private emitJob(row: JobRow): void {
    this.hub.jobUpdated(this.jobView(row));
    if (row.group_id) this.scheduleGroupEmit(row.group_id);
  }

  private emitNode(nodeId: string): void {
    const row = this.store.nodeById(nodeId);
    if (row) this.hub.nodeUpdated(this.nodeView(row));
  }

  // ---- job lifecycle -------------------------------------------------------------------------

  /** Applies a legal state transition atomically and records it in the audit log. */
  private transition(job: JobRow, to: JobState, opts: TransitionOptions): JobRow {
    if (!canTransition(job.state, to)) {
      throw new GridstatorError('CONFLICT', `Job ${job.id} cannot go from ${job.state} to ${to}`);
    }
    const now = Date.now();
    this.store.tx(() => {
      this.store.updateJob(job.id, { ...opts.patch, state: to, updated_at: now });
      this.store.insertEvent({
        job_id: job.id,
        ts: now,
        type: opts.eventType ?? 'state',
        from_state: job.state,
        to_state: to,
        actor: opts.actor,
        message: opts.message ?? null,
      });
    });
    const updated = this.store.jobById(job.id)!;
    this.emitJob(updated);
    return updated;
  }

  private note(job: JobRow, type: string, actor: string, message: string | null): void {
    this.store.insertEvent({
      job_id: job.id,
      ts: Date.now(),
      type,
      from_state: null,
      to_state: null,
      actor,
      message,
    });
  }

  submit(
    spec: JobSpec,
    principal: Principal,
    lineage?: { attempt: number; retryOf: string; retriesLeft: number },
    shard?: { groupId: string; index: number; total: number },
  ): Job {
    if (spec.snapshot) {
      if (!this.store.snapshotById(spec.snapshot)) {
        throw new GridstatorError(
          'BAD_REQUEST',
          `Snapshot ${spec.snapshot} has not been uploaded`,
          {
            hint: 'Upload it first (the CLI does this automatically).',
          },
        );
      }
      this.store.touchSnapshot(spec.snapshot, Date.now());
    }
    if (spec.target.node !== undefined) this.findNode(spec.target.node);
    const now = Date.now();
    const row: JobRow = {
      id: newId(now),
      name: jobDisplayName(spec),
      kind: spec.kind,
      spec,
      state: 'created',
      state_reason: null,
      priority: spec.priority,
      owner: principal.name,
      snapshot_id: spec.snapshot ?? null,
      project: spec.project ?? null,
      node_id: null,
      node_name: null,
      assignment_id: null,
      attempt: lineage?.attempt ?? 1,
      retry_of: lineage?.retryOf ?? null,
      retries_left: lineage?.retriesLeft ?? spec.retry.max,
      exit_code: null,
      error: null,
      log_seq: 0,
      created_at: now,
      queued_at: null,
      assigned_at: null,
      started_at: null,
      finished_at: null,
      cancel_requested_at: null,
      updated_at: now,
      outputs: null,
      group_id: shard?.groupId ?? null,
      shard_index: shard?.index ?? null,
      shard_total: shard?.total ?? null,
    };
    let queued!: JobRow;
    this.store.tx(() => {
      this.store.insertJob(row);
      this.store.insertEvent({
        job_id: row.id,
        ts: now,
        type: 'state',
        from_state: null,
        to_state: 'created',
        actor: principal.name,
        message: lineage ? `retry of ${lineage.retryOf}` : null,
      });
      queued = this.transition(row, 'queued', {
        actor: 'controller',
        patch: { queued_at: now, state_reason: 'waiting for scheduler' },
      });
    });
    this.log.info({ jobId: row.id, owner: principal.name, name: row.name }, 'job submitted');
    this.requestSchedule();
    return this.jobView(queued);
  }

  cancel(jobId: string, actor: string): Job {
    const job = this.getJob(jobId);
    if (job.state === 'cancelled') return this.jobView(job);
    if (isTerminal(job.state)) {
      throw new GridstatorError('CONFLICT', `Job ${jobId} already ${job.state}`);
    }
    if (job.state === 'created' || job.state === 'queued') {
      return this.jobView(
        this.finish(job, 'cancelled', { actor, message: 'cancelled before it started' }),
      );
    }
    const rt = job.node_id ? this.runtime(job.node_id) : undefined;
    let updated = job;
    if (job.cancel_requested_at === null) {
      const now = Date.now();
      this.store.updateJob(job.id, {
        cancel_requested_at: now,
        state_reason: 'cancelling',
        updated_at: now,
      });
      this.note(job, 'cancel-requested', actor, null);
      updated = this.store.jobById(job.id)!;
      this.emitJob(updated);
    }
    if (rt?.conn && rt.welcomed) {
      rt.conn.send({ type: 'job.cancel', jobId: job.id, reason: `cancelled by ${actor}` });
    } else {
      updated = this.finish(updated, 'cancelled', {
        actor,
        message: 'node offline; the agent will clean up when it reconnects',
      });
    }
    return this.jobView(updated);
  }

  retry(jobId: string, principal: Principal): Job {
    const job = this.getJob(jobId);
    if (!isTerminal(job.state))
      throw new GridstatorError('CONFLICT', `Job ${jobId} is still ${job.state}; cancel it first`);
    if (job.spec.snapshot && !this.store.snapshotById(job.spec.snapshot)) {
      throw new GridstatorError(
        'CONFLICT',
        `The snapshot for job ${jobId} has been garbage-collected; run the command again`,
      );
    }
    const created = this.submit(
      job.spec,
      principal,
      { attempt: job.attempt + 1, retryOf: job.id, retriesLeft: job.spec.retry.max },
      shardOf(job),
    );
    this.note(job, 'retried', principal.name, `retried as ${created.id}`);
    return created;
  }

  /** Moves a job to a terminal state, releases its reservation, and applies the retry policy. */
  private finish(job: JobRow, to: JobState, opts: TransitionOptions): JobRow {
    const now = Date.now();
    const done = this.transition(job, to, {
      ...opts,
      patch: { state_reason: null, finished_at: now, ...opts.patch },
    });
    this.rejections.delete(job.id);
    this.hub.logsEnded(this.jobView(done));
    this.log.info(
      { jobId: job.id, state: to, node: job.node_name, exitCode: done.exit_code },
      'job finished',
    );
    if (
      (to === 'lost' || to === 'failed') &&
      done.cancel_requested_at === null &&
      done.retries_left > 0 &&
      done.spec.retry.on.includes(to)
    ) {
      const principal: Principal = {
        tokenId: '',
        kind: 'client',
        name: done.owner,
        role: null,
        nodeId: null,
      };
      try {
        const next = this.submit(
          done.spec,
          principal,
          { attempt: done.attempt + 1, retryOf: done.id, retriesLeft: done.retries_left - 1 },
          shardOf(done),
        );
        this.note(done, 'retried', 'controller', `automatic retry (${to}) as ${next.id}`);
      } catch (err) {
        this.note(done, 'retry-failed', 'controller', (err as Error).message);
      }
    }
    this.requestSchedule();
    return done;
  }

  private requeue(job: JobRow, reason: string, actor: string): JobRow {
    return this.transition(job, 'queued', {
      actor,
      message: reason,
      patch: {
        node_id: null,
        node_name: null,
        assignment_id: null,
        assigned_at: null,
        queued_at: job.queued_at ?? Date.now(),
        state_reason: reason,
      },
    });
  }

  // ---- job groups (sharding) ----------------------------------------------------------------

  private groupEmitQueued = new Set<string>();

  private scheduleGroupEmit(groupId: string): void {
    if (this.groupEmitQueued.has(groupId) || this.stopped) return;
    this.groupEmitQueued.add(groupId);
    setImmediate(() => {
      this.groupEmitQueued.delete(groupId);
      if (this.stopped) return;
      const view = this.groupViewById(groupId);
      if (view) this.hub.groupUpdated(view);
    });
  }

  /** Latest attempt of each shard. */
  private latestShards(groupId: string): JobRow[] {
    const latest = new Map<number, JobRow>();
    for (const j of this.store.jobsInGroup(groupId)) {
      const idx = j.shard_index ?? 0;
      const prev = latest.get(idx);
      if (!prev || j.id > prev.id) latest.set(idx, j);
    }
    return [...latest.values()].sort((a, b) => (a.shard_index ?? 0) - (b.shard_index ?? 0));
  }

  groupView(row: GroupRow): JobGroup {
    const jobs = this.latestShards(row.id);
    const counts: Partial<Record<JobState, number>> = {};
    for (const j of jobs) counts[j.state] = (counts[j.state] ?? 0) + 1;
    const started = jobs.map((j) => j.started_at).filter((t): t is number => t !== null);
    const state = groupState(jobs.map((j) => j.state));
    const finished = jobs.map((j) => j.finished_at).filter((t): t is number => t !== null);
    return {
      id: row.id,
      name: row.name,
      owner: row.owner,
      shardTotal: row.shard_total,
      state,
      jobs: jobs.map((j) => this.jobView(j)),
      counts,
      nodes: [
        ...new Set(jobs.map((j) => j.node_name).filter((n): n is string => n !== null)),
      ].sort(),
      createdAt: row.created_at,
      startedAt: started.length ? Math.min(...started) : null,
      finishedAt:
        state === 'queued' || state === 'running' || finished.length === 0
          ? null
          : Math.max(...finished),
    };
  }

  groupViewById(id: string): JobGroup | undefined {
    const row = this.store.groupById(id);
    return row ? this.groupView(row) : undefined;
  }

  getGroup(id: string): JobGroup {
    const view = this.groupViewById(id);
    if (!view) throw new GridstatorError('NOT_FOUND', `Job group ${id} not found`);
    return view;
  }

  /** Splits a command job into `shards` jobs that run in parallel, spread across nodes. */
  submitGroup(
    spec: JobSpec,
    shards: number,
    name: string | undefined,
    principal: Principal,
  ): JobGroup {
    if (spec.kind !== 'command') {
      throw new GridstatorError('BAD_REQUEST', 'Only command jobs can be sharded', {
        hint: 'Run the test command with --shards; builds and workflows run as single jobs.',
      });
    }
    const now = Date.now();
    const group: GroupRow = {
      id: newId(now),
      name: name ?? jobDisplayName(spec),
      owner: principal.name,
      shard_total: shards,
      created_at: now,
    };
    this.store.tx(() => {
      this.store.insertGroup(group);
      for (let i = 1; i <= shards; i++)
        this.submit(shardSpec(spec, i, shards), principal, undefined, {
          groupId: group.id,
          index: i,
          total: shards,
        });
    });
    this.log.info({ groupId: group.id, shards, owner: principal.name }, 'job group submitted');
    const view = this.groupView(group);
    this.hub.groupUpdated(view);
    return view;
  }

  cancelGroup(id: string, actor: string): JobGroup {
    const group = this.getGroup(id);
    for (const j of group.jobs) if (!isTerminal(j.state)) this.cancel(j.id, actor);
    return this.getGroup(id);
  }

  /** Retries the shards that did not succeed (failed, lost or cancelled). */
  retryGroup(id: string, principal: Principal): JobGroup {
    const group = this.getGroup(id);
    if (group.jobs.some((j) => !isTerminal(j.state))) {
      throw new GridstatorError(
        'CONFLICT',
        `Group ${id} still has running shards; cancel it first`,
      );
    }
    const failed = group.jobs.filter((j) => j.state !== 'succeeded');
    if (failed.length === 0)
      throw new GridstatorError('CONFLICT', `Every shard of group ${id} succeeded`);
    for (const j of failed) this.retry(j.id, principal);
    return this.getGroup(id);
  }

  // ---- scheduling ----------------------------------------------------------------------------

  requestSchedule(): void {
    if (this.scheduleQueued || this.stopped) return;
    this.scheduleQueued = true;
    setImmediate(() => {
      this.scheduleQueued = false;
      try {
        this.runSchedule();
      } catch (err) {
        this.log.error({ err }, 'scheduling pass failed');
      }
    });
  }

  private runSchedule(): void {
    const queued = this.store.jobsInStates(['queued']);
    if (queued.length === 0) return;
    const allocs = this.allocations();
    const now = Date.now();
    const groupsByNode = this.store.activeGroupsByNode();
    const nodes: SchedulerNode[] = this.store.listNodes().map((n) => {
      const rt = this.runtime(n.id);
      const a = allocs.get(n.id);
      return {
        id: n.id,
        name: n.name,
        online: rt.conn !== undefined && rt.welcomed,
        info: n.info,
        metrics: rt.metrics,
        // A node draining or restarting for an update takes no new jobs.
        settings: this.updates?.isHeld(n.id)
          ? { ...n.settings, schedulingMode: 'draining' as const }
          : n.settings,
        effectiveLabels: [...automaticLabels(n.info), ...n.settings.labels],
        allocated: { cpus: a?.cpus ?? 0, memoryMb: a?.memoryMb ?? 0, jobs: a?.jobs.length ?? 0 },
        groups: groupsByNode.get(n.id) ?? new Map<string, number>(),
      };
    });
    const warmCache = new Map<string, Set<string>>();
    const jobs: SchedulerJob[] = queued.map((j) => {
      const rej = this.rejections.get(j.id);
      const excluded = rej
        ? new Set([...rej].filter(([, exp]) => exp > now).map(([n]) => n))
        : undefined;
      return {
        id: j.id,
        kind: j.spec.kind,
        priority: j.priority,
        queuedAt: j.queued_at ?? j.created_at,
        resources: j.spec.resources,
        target: j.spec.target,
        project: j.project ?? undefined,
        excludedNodes: excluded,
        group: j.group_id ?? undefined,
      };
    });
    const result = schedule(jobs, nodes, {
      warmNodes: (project) => {
        let set = warmCache.get(project);
        if (!set) {
          set = new Set(this.store.recentNodesForProject(project, now - WARM_WINDOW_MS));
          warmCache.set(project, set);
        }
        return set;
      },
    });
    const byId = new Map(queued.map((j) => [j.id, j]));
    for (const a of result.assignments) {
      const job = byId.get(a.jobId);
      if (job) this.dispatch(job, a.nodeId);
    }
    for (const [jobId, reason] of result.pending) {
      const job = byId.get(jobId);
      if (job && job.state_reason !== reason) {
        this.store.updateJob(jobId, { state_reason: reason });
        this.emitJob({ ...job, state_reason: reason });
      }
    }
  }

  private dispatch(job: JobRow, nodeId: string): void {
    const node = this.store.nodeById(nodeId);
    const rt = this.runtime(nodeId);
    if (!node || !rt.conn) return;
    const assignmentId = newId();
    const assigned = this.transition(job, 'assigned', {
      actor: 'scheduler',
      message: `assigned to ${node.name}`,
      patch: {
        node_id: nodeId,
        node_name: node.name,
        assignment_id: assignmentId,
        assigned_at: Date.now(),
        state_reason: null,
      },
    });
    const sent = rt.conn.send({
      type: 'job.assign',
      jobId: job.id,
      assignmentId,
      attempt: job.attempt,
      spec: job.spec,
      snapshotId: job.snapshot_id,
    });
    if (!sent) this.requeue(assigned, `could not deliver the job to ${node.name}`, 'controller');
    else this.log.info({ jobId: job.id, node: node.name }, 'job assigned');
  }

  // ---- node registration & settings ----------------------------------------------------------

  registerNode(
    name: string,
    info: NodeInfo,
    patch: NodeSettingsPatch | undefined,
    joinTokenId: string | null,
  ): { nodeId: string; name: string; credential: string } {
    if (this.store.nodeByName(name) || this.store.nodeById(name)) {
      throw new GridstatorError('CONFLICT', `A node named "${name}" is already registered`, {
        hint: `Pick another name with --name, or remove the old node with \`gridstator nodes remove ${name}\`.`,
      });
    }
    const nodeId = newId();
    const settings = mergeSettings(defaultNodeSettings(info), patch);
    let credential = '';
    this.store.tx(() => {
      this.store.insertNode({
        id: nodeId,
        name,
        info,
        settings,
        created_at: Date.now(),
        last_seen_at: null,
      });
      if (joinTokenId) this.store.consumeJoinToken(joinTokenId);
      credential = this.auth.issue({ kind: 'agent', name: `agent:${name}`, nodeId }).token;
    });
    this.log.info({ nodeId, name }, 'node registered');
    this.emitNode(nodeId);
    return { nodeId, name, credential };
  }

  updateNodeSettings(idOrName: string, patch: PatchNodeRequest): Node {
    const { name, ...settingsPatch } = patch;
    const { node, settings } = this.store.tx(() => {
      const node = this.findNode(idOrName);
      const settings = mergeSettings(node.settings, settingsPatch);
      if (name !== undefined) {
        const collision = [this.store.nodeByName(name), this.store.nodeById(name)].some(
          (other) => other !== undefined && other.id !== node.id,
        );
        if (collision)
          throw new GridstatorError(
            'CONFLICT',
            `A node named "${name}" is already registered or the name is another node's ID`,
          );
        if (name !== node.name) this.store.rebindNodeTargets(node.name, node.id);
      }
      this.store.updateNode(node.id, { ...(name === undefined ? {} : { name }), settings });
      return { node, settings };
    });
    const rt = this.runtime(node.id);
    if (rt.conn && rt.welcomed)
      rt.conn.send({ type: 'node.settings', settings, ...(name === undefined ? {} : { name }) });
    this.log.info({ node: name ?? node.name, settings }, 'node updated');
    this.emitNode(node.id);
    this.requestSchedule();
    if (patch.autoUpdate !== undefined) this.updates?.tick();
    return this.nodeView(this.store.nodeById(node.id)!);
  }

  removeNode(idOrName: string, force: boolean, actor: string): void {
    const node = this.findNode(idOrName);
    const active = this.store.jobsOnNode(node.id, DISPATCHED);
    if (active.length > 0 && !force) {
      throw new GridstatorError(
        'CONFLICT',
        `Node ${node.name} is running ${active.length} job(s)`,
        {
          hint: 'Drain it first (`gridstator nodes drain`) or pass --force to cancel them.',
        },
      );
    }
    for (const job of active)
      this.finish(job, 'cancelled', { actor, message: `node ${node.name} was removed` });
    const rt = this.runtimes.get(node.id);
    rt?.conn?.send({
      type: 'error',
      code: 'NODE_REMOVED',
      message: 'This node was removed from the cluster',
      fatal: true,
    });
    rt?.conn?.close(4003, 'node removed');
    this.store.tx(() => {
      this.store.revokeNodeTokens(node.id, Date.now());
      this.store.deleteNode(node.id);
    });
    this.runtimes.delete(node.id);
    this.hub.nodeRemoved(node.id);
    this.log.info({ node: node.name, actor }, 'node removed');
  }

  // ---- agent channel -------------------------------------------------------------------------

  agentConnected(nodeId: string, conn: AgentConnection): void {
    if (this.stopped) {
      conn.close(1001, 'controller shutting down');
      return;
    }
    const rt = this.runtime(nodeId);
    if (rt.conn && rt.conn !== conn) rt.conn.close(4001, 'replaced by a newer connection');
    rt.conn = conn;
    rt.welcomed = false;
    rt.connectedAt = Date.now();
    rt.lastHeartbeatAt = Date.now();
  }

  agentDisconnected(nodeId: string, conn: AgentConnection): void {
    if (this.stopped) return;
    const rt = this.runtimes.get(nodeId);
    if (!rt || rt.conn !== conn) return;
    rt.conn = undefined;
    rt.welcomed = false;
    rt.disconnectedAt = Date.now();
    rt.metrics = null;
    this.log.warn({ nodeId }, 'agent disconnected');
    this.emitNode(nodeId);
  }

  agentMessage(nodeId: string, conn: AgentConnection, msg: AgentMessage): void {
    if (this.stopped) return;
    const rt = this.runtime(nodeId);
    if (rt.conn !== conn) return; // stale connection
    rt.lastHeartbeatAt = Date.now();
    if (msg.type !== 'hello' && !rt.welcomed) {
      this.log.warn({ nodeId, type: msg.type }, 'agent message before hello ignored');
      return;
    }
    switch (msg.type) {
      case 'hello':
        return this.onHello(nodeId, conn, msg);
      case 'heartbeat':
        return this.onHeartbeat(nodeId, conn, msg.metrics, msg.activeJobs);
      case 'job.state':
        return this.onJobState(nodeId, msg.jobId, msg.assignmentId, msg.phase, msg.ts);
      case 'job.log':
        return this.onJobLog(nodeId, msg.jobId, msg.assignmentId, msg.entries);
      case 'job.result':
        return this.onJobResult(nodeId, conn, msg.result);
      case 'job.rejected':
        return this.onJobRejected(nodeId, msg.jobId, msg.assignmentId, msg.reason);
      case 'job.outputs':
        return this.onJobOutputs(nodeId, msg.jobId, msg.assignmentId, msg.outputs);
    }
  }

  private onHello(nodeId: string, conn: AgentConnection, hello: AgentHello): void {
    const rt = this.runtime(nodeId);
    const node = this.store.nodeById(nodeId);
    if (!node) {
      conn.send({
        type: 'error',
        code: 'NODE_REMOVED',
        message: 'This node is not registered',
        fatal: true,
      });
      conn.close(4003, 'unknown node');
      return;
    }
    if (hello.protocolVersion !== AGENT_PROTOCOL_VERSION) {
      conn.send({
        type: 'error',
        code: 'PROTOCOL_MISMATCH',
        message: `Agent protocol v${hello.protocolVersion} is not compatible with controller protocol v${AGENT_PROTOCOL_VERSION}; managed agents update themselves, others need the controller's version installed`,
        fatal: true,
      });
      conn.close(4002, 'protocol mismatch');
      return;
    }
    const settings = hello.settings ? mergeSettings(node.settings, hello.settings) : node.settings;
    this.store.updateNode(nodeId, { info: hello.info, settings, last_seen_at: Date.now() });

    const agentActive = new Map(hello.activeJobs.map((a) => [a.jobId, a]));
    const agentResults = new Map(hello.results.map((r) => [r.jobId, r]));
    const resume: Array<{ jobId: string; logSeq: number }> = [];
    const matched = new Set<string>();
    for (const job of this.store.jobsOnNode(nodeId, DISPATCHED)) {
      const active = agentActive.get(job.id);
      const result = agentResults.get(job.id);
      if (active && active.assignmentId === job.assignment_id) {
        matched.add(job.id);
        let current = job;
        if (active.phase === 'running' && current.state !== 'running') {
          current = this.transition(current, 'running', {
            actor: `node:${node.name}`,
            message: 'reported running after reconnect',
            patch: { started_at: current.started_at ?? Date.now() },
          });
        } else if (active.phase === 'starting' && current.state === 'assigned') {
          current = this.transition(current, 'starting', { actor: `node:${node.name}` });
        }
        resume.push({ jobId: job.id, logSeq: current.log_seq });
      } else if (result && result.assignmentId === job.assignment_id) {
        matched.add(job.id);
        resume.push({ jobId: job.id, logSeq: job.log_seq });
      } else if (job.state === 'assigned') {
        this.requeue(job, `${node.name} no longer has the job (agent restarted?)`, 'controller');
      } else {
        this.finish(job, 'lost', {
          actor: 'controller',
          message: `${node.name} reconnected without the job`,
          patch: { error: `The agent on ${node.name} restarted or lost track of the job` },
        });
      }
    }
    rt.welcomed = true;
    rt.metrics = null;
    const controllerUrl = this.controllerEndpoint?.();
    conn.send({
      type: 'welcome',
      nodeId,
      name: node.name,
      settings,
      resume,
      ...(controllerUrl ? { controllerUrl } : {}),
    });
    for (const active of hello.activeJobs) {
      if (!matched.has(active.jobId)) {
        conn.send({
          type: 'job.cancel',
          jobId: active.jobId,
          reason: 'the job is no longer assigned to this node',
        });
      }
    }
    for (const result of hello.results) {
      if (!matched.has(result.jobId)) conn.send({ type: 'job.ack', jobId: result.jobId });
    }
    this.log.info(
      { node: node.name, active: hello.activeJobs.length, pendingResults: hello.results.length },
      'agent connected',
    );
    this.emitNode(nodeId);
    this.requestSchedule();
    this.updates?.agentJoined(nodeId);
  }

  private onHeartbeat(
    nodeId: string,
    conn: AgentConnection,
    metrics: NodeMetrics,
    activeJobs: string[],
  ): void {
    const rt = this.runtime(nodeId);
    rt.metrics = metrics;
    this.store.updateNode(nodeId, { last_seen_at: Date.now() });
    for (const jobId of activeJobs) {
      const job = this.store.jobById(jobId);
      if (!job || job.node_id !== nodeId || isTerminal(job.state) || job.state === 'queued') {
        conn.send({
          type: 'job.cancel',
          jobId,
          reason: 'the job is no longer assigned to this node',
        });
      }
    }
    this.emitNode(nodeId);
  }

  /** Returns the job if the report is for the current assignment on this node, else undefined. */
  private currentAssignment(
    nodeId: string,
    jobId: string,
    assignmentId: string,
  ): JobRow | undefined {
    const job = this.store.jobById(jobId);
    if (!job || job.node_id !== nodeId || job.assignment_id !== assignmentId) {
      this.log.debug({ jobId, nodeId, assignmentId }, 'ignoring report for stale assignment');
      return undefined;
    }
    return job;
  }

  private onJobState(
    nodeId: string,
    jobId: string,
    assignmentId: string,
    phase: 'starting' | 'running',
    ts: number,
  ): void {
    const job = this.currentAssignment(nodeId, jobId, assignmentId);
    if (!job || isTerminal(job.state) || job.state === phase) return;
    if (phase === 'starting' && job.state !== 'assigned') return;
    this.transition(job, phase, {
      actor: `node:${job.node_name}`,
      ...(phase === 'running' ? { patch: { started_at: ts } } : {}),
    });
  }

  private onJobLog(nodeId: string, jobId: string, assignmentId: string, entries: LogEntry[]): void {
    const job = this.currentAssignment(nodeId, jobId, assignmentId);
    if (!job || isTerminal(job.state)) return;
    const fresh = entries.filter((e) => e.s > job.log_seq).sort((a, b) => a.s - b.s);
    if (fresh.length === 0) return;
    const written = this.logs.append(jobId, fresh);
    this.store.updateJob(jobId, { log_seq: fresh[fresh.length - 1]!.s });
    this.hub.logs(jobId, written);
  }

  private onJobResult(nodeId: string, conn: AgentConnection, result: AgentJobResult): void {
    const job = this.store.jobById(result.jobId);
    if (
      !job ||
      job.node_id !== nodeId ||
      job.assignment_id !== result.assignmentId ||
      isTerminal(job.state)
    ) {
      conn.send({ type: 'job.ack', jobId: result.jobId });
      return;
    }
    if (result.logSeq > job.log_seq) {
      this.log.warn(
        { jobId: job.id, have: job.log_seq, agent: result.logSeq },
        'result arrived before all log entries',
      );
    }
    const to: JobState =
      result.outcome === 'succeeded'
        ? 'succeeded'
        : result.outcome === 'cancelled' || job.cancel_requested_at !== null
          ? 'cancelled'
          : result.outcome === 'lost'
            ? 'lost'
            : 'failed';
    this.store.tx(() => {
      if (result.outputs) this.store.setJobOutputs(job.id, result.outputs);
      if (result.artifacts.length > 0)
        this.store.insertArtifacts(job.id, result.artifacts, Date.now());
    });
    this.finish(job, to, {
      actor: `node:${job.node_name}`,
      message: result.error,
      patch: {
        exit_code: result.exitCode,
        error: to === 'cancelled' ? null : result.error,
        finished_at: result.finishedAt,
      },
    });
    conn.send({ type: 'job.ack', jobId: job.id });
  }

  private onJobOutputs(
    nodeId: string,
    jobId: string,
    assignmentId: string,
    outputs: JobOutputs,
  ): void {
    const job = this.currentAssignment(nodeId, jobId, assignmentId);
    if (!job || isTerminal(job.state)) return;
    this.store.setJobOutputs(jobId, outputs);
    this.emitJob({ ...job, outputs });
  }

  private onJobRejected(nodeId: string, jobId: string, assignmentId: string, reason: string): void {
    const job = this.currentAssignment(nodeId, jobId, assignmentId);
    if (!job || isTerminal(job.state)) return;
    let rej = this.rejections.get(jobId);
    if (!rej) this.rejections.set(jobId, (rej = new Map<string, number>()));
    rej.set(nodeId, Date.now() + REJECTION_TTL_MS);
    if (job.state === 'assigned' || job.state === 'starting') {
      this.transition(job, 'queued', {
        actor: `node:${job.node_name}`,
        message: `rejected: ${reason}`,
        patch: {
          node_id: null,
          node_name: null,
          assignment_id: null,
          assigned_at: null,
          state_reason: `${job.node_name} rejected the job: ${reason}`,
        },
      });
    } else {
      this.finish(job, 'failed', {
        actor: `node:${job.node_name}`,
        message: reason,
        patch: { error: reason },
      });
    }
    this.requestSchedule();
  }

  // ---- periodic reconciliation ---------------------------------------------------------------

  sweep(): void {
    if (this.stopped) return;
    const now = Date.now();
    try {
      for (const node of this.store.listNodes()) {
        const rt = this.runtime(node.id);
        if (
          rt.conn &&
          rt.lastHeartbeatAt !== null &&
          now - rt.lastHeartbeatAt > this.timing.heartbeatTimeoutMs
        ) {
          this.log.warn({ node: node.name }, 'agent heartbeat timed out; closing connection');
          const conn = rt.conn;
          conn.close(4000, 'heartbeat timeout');
          this.agentDisconnected(node.id, conn);
        }
        if (!rt.conn && now - rt.disconnectedAt > this.timing.lostTimeoutMs) {
          for (const job of this.store.jobsOnNode(node.id, DISPATCHED)) {
            if (job.state === 'assigned') {
              this.requeue(job, `${node.name} went offline before starting the job`, 'controller');
            } else if (job.cancel_requested_at !== null) {
              this.finish(job, 'cancelled', {
                actor: 'controller',
                message: `${node.name} is offline`,
              });
            } else {
              this.finish(job, 'lost', {
                actor: 'controller',
                message: `${node.name} has been offline for ${Math.round((now - rt.disconnectedAt) / 1000)}s`,
                patch: { error: `Lost contact with ${node.name}` },
              });
            }
          }
        }
      }
      for (const job of this.store.jobsInStates(DISPATCHED)) {
        if (
          job.cancel_requested_at !== null &&
          now - job.cancel_requested_at > this.timing.cancelTimeoutMs
        ) {
          this.finish(job, 'cancelled', {
            actor: 'controller',
            message: 'the agent did not confirm cancellation in time',
          });
          continue;
        }
        if (
          job.state === 'assigned' &&
          job.assigned_at !== null &&
          now - job.assigned_at > this.timing.assignTimeoutMs &&
          job.node_id
        ) {
          const rt = this.runtime(job.node_id);
          if (rt.conn && rt.welcomed) {
            rt.conn.send({ type: 'job.cancel', jobId: job.id, reason: 'assignment timed out' });
            this.requeue(job, `${job.node_name} did not acknowledge the job`, 'controller');
          }
        }
      }
      for (const [jobId, rej] of this.rejections) {
        for (const [nodeId, exp] of rej) if (exp < now) rej.delete(nodeId);
        if (rej.size === 0) this.rejections.delete(jobId);
      }
      this.updates?.tick();
    } catch (err) {
      this.log.error({ err }, 'sweep failed');
    }
  }
}

export function mergeSettings(
  base: NodeSettings,
  patch: NodeSettingsPatch | undefined,
): NodeSettings {
  if (!patch) return base;
  const merged = {
    maxConcurrency: patch.maxConcurrency ?? base.maxConcurrency,
    reserved: {
      cpus: patch.reserved?.cpus ?? base.reserved.cpus,
      memoryMb: patch.reserved?.memoryMb ?? base.reserved.memoryMb,
    },
    labels: patch.labels ?? base.labels,
    schedulingMode: patch.schedulingMode ?? base.schedulingMode,
    autoUpdate: patch.autoUpdate ?? base.autoUpdate ?? 'inherit',
  };
  return NodeSettingsSchema.parse(merged);
}

function shardOf(job: JobRow): { groupId: string; index: number; total: number } | undefined {
  return job.group_id && job.shard_index && job.shard_total
    ? { groupId: job.group_id, index: job.shard_index, total: job.shard_total }
    : undefined;
}

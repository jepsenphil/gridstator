import type {
  JobKind,
  JobTarget,
  NodeInfo,
  NodeMetrics,
  NodeSettings,
  ResourceRequest,
} from '@gridstator/protocol';

export interface SchedulerNode {
  id: string;
  name: string;
  online: boolean;
  info: NodeInfo | null;
  metrics: NodeMetrics | null;
  settings: NodeSettings;
  effectiveLabels: readonly string[];
  /** Resources and slots currently held by dispatched jobs on this node. */
  allocated: { cpus: number; memoryMb: number; jobs: number };
}

export interface SchedulerJob {
  id: string;
  kind: JobKind;
  priority: number;
  queuedAt: number;
  resources: ResourceRequest;
  target: JobTarget;
  project?: string | undefined;
  /** Nodes that rejected this job recently; they are skipped. */
  excludedNodes?: ReadonlySet<string> | undefined;
}

export interface ScheduleContext {
  /** Node ids that recently ran a job for the given project (warm caches). */
  warmNodes?: (project: string) => ReadonlySet<string>;
}

export interface Assignment {
  jobId: string;
  nodeId: string;
  score: number;
}

export interface ScheduleResult {
  assignments: Assignment[];
  /** Why each unassigned job is still waiting, for display to users. */
  pending: Map<string, string>;
}

export function allocatable(node: Pick<SchedulerNode, 'info' | 'settings'>): {
  cpus: number;
  memoryMb: number;
} {
  if (!node.info) return { cpus: 0, memoryMb: 0 };
  return {
    cpus: Math.max(0, node.info.cpus - node.settings.reserved.cpus),
    memoryMb: Math.max(0, node.info.memoryTotalMb - node.settings.reserved.memoryMb),
  };
}

/** Rejection stages, ordered from "furthest from running" to "closest to running". */
const Stage = {
  Target: 0,
  Offline: 1,
  Mode: 2,
  Capability: 3,
  Labels: 4,
  NeverFits: 5,
  Capacity: 6,
} as const;
type Stage = (typeof Stage)[keyof typeof Stage];

interface Rejection {
  stage: Stage;
  reason: string;
}

function formatRequest(r: ResourceRequest): string {
  const gib = r.memoryMb / 1024;
  return `${r.cpus} CPU, ${gib >= 10 || Number.isInteger(gib) ? gib.toFixed(0) : gib.toFixed(1)} GiB`;
}

function checkNode(job: SchedulerJob, node: SchedulerNode): Rejection | undefined {
  if (
    job.target.node !== undefined &&
    job.target.node !== node.id &&
    job.target.node !== node.name
  ) {
    return { stage: Stage.Target, reason: `job is pinned to node "${job.target.node}"` };
  }
  if (job.excludedNodes?.has(node.id)) {
    return { stage: Stage.Capability, reason: `node ${node.name} rejected this job` };
  }
  if (!node.online || !node.info)
    return { stage: Stage.Offline, reason: `node ${node.name} is offline` };
  if (node.settings.schedulingMode !== 'enabled') {
    return { stage: Stage.Mode, reason: `node ${node.name} is ${node.settings.schedulingMode}` };
  }
  if (!node.info.executors.includes(job.kind)) {
    return {
      stage: Stage.Capability,
      reason: `node ${node.name} cannot run "${job.kind}" jobs${node.info.docker.available ? '' : ' (Docker unavailable)'}`,
    };
  }
  const missing = job.target.labels.filter((l) => !node.effectiveLabels.includes(l));
  if (missing.length > 0) {
    return {
      stage: Stage.Labels,
      reason: `node ${node.name} lacks label(s) ${missing.join(', ')}`,
    };
  }
  const cap = allocatable(node);
  if (job.resources.cpus > cap.cpus || job.resources.memoryMb > cap.memoryMb) {
    return {
      stage: Stage.NeverFits,
      reason: `job needs ${formatRequest(job.resources)} but node ${node.name} allows at most ${formatRequest(cap)}`,
    };
  }
  if (node.allocated.jobs >= node.settings.maxConcurrency) {
    return {
      stage: Stage.Capacity,
      reason: `node ${node.name} is at its concurrency limit (${node.settings.maxConcurrency})`,
    };
  }
  if (
    node.allocated.cpus + job.resources.cpus > cap.cpus ||
    node.allocated.memoryMb + job.resources.memoryMb > cap.memoryMb
  ) {
    return {
      stage: Stage.Capacity,
      reason: `waiting for ${formatRequest(job.resources)} on node ${node.name}`,
    };
  }
  if (node.metrics && node.metrics.memoryAvailableMb < job.resources.memoryMb) {
    return {
      stage: Stage.Capacity,
      reason: `node ${node.name} has only ${Math.floor(node.metrics.memoryAvailableMb / 1024)} GiB memory available right now`,
    };
  }
  return undefined;
}

function score(job: SchedulerJob, node: SchedulerNode, warm: boolean): number {
  const cap = allocatable(node);
  const freeCpu =
    cap.cpus > 0 ? (cap.cpus - node.allocated.cpus - job.resources.cpus) / cap.cpus : 0;
  const freeMem =
    cap.memoryMb > 0
      ? (cap.memoryMb - node.allocated.memoryMb - job.resources.memoryMb) / cap.memoryMb
      : 0;
  const cpuUtil = (node.metrics?.cpuPercent ?? 50) / 100;
  return freeCpu + freeMem - 0.5 * cpuUtil - 0.1 * node.allocated.jobs + (warm ? 0.5 : 0);
}

function summarize(
  job: SchedulerJob,
  nodes: readonly SchedulerNode[],
  rejections: Rejection[],
): string {
  if (nodes.length === 0) return 'no worker nodes are registered';
  if (
    job.target.node !== undefined &&
    !nodes.some((n) => n.id === job.target.node || n.name === job.target.node)
  ) {
    return `node "${job.target.node}" does not exist`;
  }
  const relevant = rejections.filter((r) => r.stage !== Stage.Target);
  if (relevant.length === 0) return 'no eligible nodes';
  const best = Math.max(...relevant.map((r) => r.stage));
  const atBest = relevant.filter((r) => r.stage === best);
  if (best === Stage.Offline)
    return job.target.node ? (atBest[0]?.reason ?? 'node offline') : 'no worker nodes are online';
  if (atBest.length > 1) {
    if (best === Stage.Capacity) return `waiting for capacity (${formatRequest(job.resources)})`;
    if (best === Stage.Labels)
      return `no eligible node has label(s) ${job.target.labels.join(', ')}`;
    if (best === Stage.NeverFits)
      return `no node is large enough for ${formatRequest(job.resources)}`;
    if (best === Stage.Mode) return 'all eligible nodes are disabled or draining';
  }
  return atBest[0]?.reason ?? 'no eligible nodes';
}

/**
 * Pure scheduling pass. Jobs are considered by priority (desc) then queue time (asc); each is
 * placed on the best-scoring eligible node. A job that cannot be placed does not block later jobs
 * (backfill).
 */
export function schedule(
  queue: readonly SchedulerJob[],
  nodes: readonly SchedulerNode[],
  context: ScheduleContext = {},
): ScheduleResult {
  const working = nodes.map((n) => ({ ...n, allocated: { ...n.allocated } }));
  const ordered = [...queue].sort(
    (a, b) => b.priority - a.priority || a.queuedAt - b.queuedAt || a.id.localeCompare(b.id),
  );
  const assignments: Assignment[] = [];
  const pending = new Map<string, string>();

  for (const job of ordered) {
    const warm = job.project && context.warmNodes ? context.warmNodes(job.project) : undefined;
    let best: { node: (typeof working)[number]; score: number } | undefined;
    const rejections: Rejection[] = [];
    for (const node of working) {
      const rejection = checkNode(job, node);
      if (rejection) {
        rejections.push(rejection);
        continue;
      }
      const s = score(job, node, warm?.has(node.id) ?? false);
      if (!best || s > best.score || (s === best.score && node.name < best.node.name)) {
        best = { node, score: s };
      }
    }
    if (!best) {
      pending.set(job.id, summarize(job, working, rejections));
      continue;
    }
    best.node.allocated.cpus += job.resources.cpus;
    best.node.allocated.memoryMb += job.resources.memoryMb;
    best.node.allocated.jobs += 1;
    assignments.push({ jobId: job.id, nodeId: best.node.id, score: best.score });
  }
  return { assignments, pending };
}

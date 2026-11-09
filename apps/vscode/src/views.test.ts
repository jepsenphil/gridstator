import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  defaultNodeSettings,
  JobSpecSchema,
  type Job,
  type JobState,
  type Node,
} from '@gridstator/protocol';
import type { ConnectionInfo } from './messages';
import { ClusterState } from './state';
import { ClusterTreeProvider, JobsTreeProvider } from './views';

vi.mock('vscode', async () => {
  const host = await import('./test-vscode');
  return {
    ...host,
    TreeItem: class {
      constructor(
        public label: string,
        public collapsibleState: number,
      ) {}
    },
    ThemeColor: class {
      constructor(public id: string) {}
    },
    ThemeIcon: class {
      constructor(
        public id: string,
        public color?: { id: string },
      ) {}
    },
    MarkdownString: class {
      constructor(public value = '') {}
      appendMarkdown(value: string) {
        this.value += value;
        return this;
      }
    },
    TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
  };
});

describe('sidebar tree refreshes', () => {
  const now = Date.now();
  const node: Node = {
    id: 'worker',
    name: 'worker',
    status: 'online',
    health: 'ok',
    healthIssues: [],
    info: null,
    metrics: {
      ts: now,
      cpuPercent: 37,
      memoryAvailableMb: 3000,
      diskFreeMb: 4000,
      diskTotalMb: 5000,
      load1: 1,
    },
    settings: defaultNodeSettings({ cpus: 4, memoryTotalMb: 4096 }),
    effectiveLabels: [],
    activeJobs: [],
    allocated: { cpus: 0, memoryMb: 0 },
    allocatable: { cpus: 3, memoryMb: 3000 },
    lastSeenAt: now,
    connectedAt: now,
    createdAt: now,
  };
  const job = (id: string, state: JobState): Job => ({
    id,
    name: id,
    state,
    kind: 'command',
    stateReason: null,
    priority: 0,
    owner: 'operator',
    spec: JobSpecSchema.parse({ kind: 'command', image: 'node:22-bookworm', command: ['true'] }),
    snapshotId: null,
    nodeId: node.id,
    nodeName: node.name,
    assignmentId: null,
    attempt: 1,
    retryOf: null,
    exitCode: null,
    error: null,
    queuePosition: null,
    logSeq: 0,
    outputs: null,
    artifacts: [],
    groupId: null,
    shardIndex: null,
    shardTotal: null,
    createdAt: now,
    queuedAt: now,
    assignedAt: null,
    startedAt: state === 'queued' ? null : now - 30_000,
    finishedAt: state === 'succeeded' ? now : null,
    cancelRequestedAt: null,
  });
  const connection: ConnectionInfo = {
    status: 'connected',
    url: 'https://controller:7300',
    user: 'operator',
    role: 'operator',
    clusterId: 'cluster',
    version: '0.1.0',
    error: null,
  };
  const initial = {
    nodes: [node],
    jobs: [job('running', 'running'), job('queued', 'queued'), job('done', 'succeeded')],
    connection,
  };
  let state: ClusterState;
  let cluster: ClusterTreeProvider;
  let jobs: JobsTreeProvider;

  beforeEach(() => {
    vi.useFakeTimers();
    state = new ClusterState(
      () => undefined,
      () => 100,
      { appendLine: () => undefined },
    );
    vi.spyOn(state, 'connected', 'get').mockReturnValue(true);
    state.connection = initial.connection;
    for (const node of initial.nodes) state.nodes.set(node.id, node);
    for (const job of initial.jobs) state.jobs.set(job.id, job);
    cluster = new ClusterTreeProvider(state);
    jobs = new JobsTreeProvider(state);
    cluster.getChildren();
    for (const group of jobs.getChildren()) jobs.getChildren(group);
  });

  afterEach(() => {
    jobs.dispose();
    cluster.dispose();
    state.dispose();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('ignores heartbeat timestamps and keeps node updates out of Jobs', () => {
    const node = initial.nodes[0]!;
    const clusterChanged = vi.fn();
    const jobsChanged = vi.fn();
    cluster.onDidChangeTreeData(clusterChanged);
    jobs.onDidChangeTreeData(jobsChanged);

    state.upsertNode({
      ...node,
      lastSeenAt: Date.now(),
      metrics: { ...node.metrics!, ts: Date.now() },
    });

    expect(clusterChanged).not.toHaveBeenCalled();
    expect(jobsChanged).not.toHaveBeenCalled();
  });

  it('updates a changed node in place without restarting either tree', () => {
    const node = initial.nodes[0]!;
    const handle = cluster.getChildren().find((item) => 'id' in item && item.id === node.id)!;
    const clusterChanged = vi.fn();
    const jobsChanged = vi.fn();
    cluster.onDidChangeTreeData(clusterChanged);
    jobs.onDidChangeTreeData(jobsChanged);

    state.upsertNode({ ...node, metrics: { ...node.metrics!, cpuPercent: 64 } });

    expect(clusterChanged.mock.calls).toEqual([[handle]]);
    expect(jobsChanged).not.toHaveBeenCalled();
    expect(cluster.getChildren().find((item) => 'id' in item && item.id === node.id)).toBe(handle);
    expect(cluster.getTreeItem(handle).description).toContain('64% CPU');
  });

  it('ignores job log progress and refreshes only a changed job label', () => {
    const job = initial.jobs.find((item) => item.state === 'running')!;
    const group = jobs.getChildren().find((item) => 'group' in item && item.group === 'active')!;
    const handle = jobs.getChildren(group)[0]!;
    const jobsChanged = vi.fn();
    const clusterChanged = vi.fn();
    jobs.onDidChangeTreeData(jobsChanged);
    cluster.onDidChangeTreeData(clusterChanged);

    state.upsertJob({ ...job, logSeq: job.logSeq + 1 });
    expect(jobsChanged).not.toHaveBeenCalled();
    state.upsertJob({ ...job, name: 'Updated command' });

    expect(jobsChanged.mock.calls).toEqual([[handle]]);
    expect(clusterChanged).not.toHaveBeenCalled();
    expect(jobs.getTreeItem(handle).label).toBe('Updated command');
    expect(jobs.getTreeItem(handle).command?.arguments?.[0]).toMatchObject({
      name: 'Updated command',
    });
  });

  it('ticks running durations without rebuilding groups or completed rows', () => {
    const groups = jobs.getChildren();
    const active = groups.find((item) => 'group' in item && item.group === 'active')!;
    const handle = jobs.getChildren(active)[0]!;
    const changed = vi.fn();
    jobs.onDidChangeTreeData(changed);

    vi.advanceTimersByTime(2000);

    expect(changed.mock.calls).toEqual([[handle]]);
    expect(jobs.getChildren()[0]).toBe(groups[0]);
    expect(jobs.getChildren(active)[0]).toBe(handle);
  });

  it('still refreshes structure when jobs move between groups', () => {
    const queued = initial.jobs.find((item) => item.state === 'queued')!;
    const queuedGroup = jobs
      .getChildren()
      .find((item) => 'group' in item && item.group === 'queued')!;
    const handle = jobs.getChildren(queuedGroup)[0]!;
    const changed = vi.fn();
    jobs.onDidChangeTreeData(changed);

    state.upsertJob({ ...queued, state: 'running', startedAt: Date.now() });

    expect(changed.mock.calls).toEqual([[undefined]]);
    expect(jobs.getChildren().some((item) => 'group' in item && item.group === 'queued')).toBe(
      false,
    );
    const active = jobs.getChildren().find((item) => 'group' in item && item.group === 'active')!;
    expect(jobs.getTreeItem(active).label).toBe('Running (2)');
    expect(jobs.getChildren(active)).toContain(handle);
    expect(jobs.getTreeItem(handle).description).toContain('running');
  });

  it('refreshes only Recent when another finished job arrives', () => {
    const recent = jobs.getChildren().find((item) => 'group' in item && item.group === 'recent')!;
    const done = initial.jobs.find((item) => item.state === 'succeeded')!;
    const changed = vi.fn();
    jobs.onDidChangeTreeData(changed);

    state.upsertJob({ ...done, id: 'second-done', name: 'Another completed job' });

    expect(changed.mock.calls).toEqual([[recent]]);
    expect(jobs.getChildren(recent)).toHaveLength(2);
  });

  it('still refreshes Cluster when a worker joins', () => {
    const handle = cluster.getChildren()[0]!;
    const changed = vi.fn();
    cluster.onDidChangeTreeData(changed);

    state.upsertNode({ ...node, id: 'second-worker', name: 'Second worker' });

    expect(changed.mock.calls).toEqual([[undefined]]);
    expect(cluster.getChildren()).toHaveLength(2);
    expect(cluster.getChildren()).toContain(handle);
  });
});

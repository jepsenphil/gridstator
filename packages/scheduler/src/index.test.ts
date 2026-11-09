import { describe, expect, it } from 'vitest';
import type { NodeInfo } from '@gridstator/protocol';
import { schedule, type SchedulerJob, type SchedulerNode } from './index.js';

function info(cpus: number, memGb: number, overrides: Partial<NodeInfo> = {}): NodeInfo {
  return {
    hostname: 'h',
    platform: 'linux',
    osRelease: '6',
    arch: 'x64',
    cpus,
    memoryTotalMb: memGb * 1024,
    agentVersion: '0.1.0',
    docker: { available: true },
    buildx: { available: true },
    act: { available: false },
    executors: ['command'],
    ...overrides,
  };
}

function node(
  id: string,
  opts: Partial<SchedulerNode> & { cpus?: number; memGb?: number } = {},
): SchedulerNode {
  const { cpus = 16, memGb = 64, ...rest } = opts;
  return {
    id,
    name: id,
    online: true,
    info: info(cpus, memGb),
    metrics: null,
    settings: {
      maxConcurrency: 4,
      reserved: { cpus: 0, memoryMb: 0 },
      labels: [],
      schedulingMode: 'enabled',
    },
    effectiveLabels: ['os=linux', 'arch=x64', 'docker'],
    allocated: { cpus: 0, memoryMb: 0, jobs: 0 },
    ...rest,
  };
}

let seq = 0;
function job(overrides: Partial<SchedulerJob> = {}): SchedulerJob {
  seq++;
  return {
    id: `job${String(seq).padStart(3, '0')}`,
    kind: 'command',
    priority: 0,
    queuedAt: seq,
    resources: { cpus: 2, memoryMb: 4096 },
    target: { labels: [] },
    ...overrides,
  };
}

describe('schedule', () => {
  it('assigns a job to an eligible node', () => {
    const j = job();
    const r = schedule([j], [node('a')]);
    expect(r.assignments).toEqual([expect.objectContaining({ jobId: j.id, nodeId: 'a' })]);
    expect(r.pending.size).toBe(0);
  });

  it('reports a clear reason when no nodes exist or none are online', () => {
    const j = job();
    expect(schedule([j], []).pending.get(j.id)).toBe('no worker nodes are registered');
    expect(schedule([j], [node('a', { online: false })]).pending.get(j.id)).toBe(
      'no worker nodes are online',
    );
  });

  it('prefers the less loaded node', () => {
    const busy = node('busy', { allocated: { cpus: 10, memoryMb: 40_000, jobs: 3 } });
    const idle = node('idle');
    const r = schedule([job()], [busy, idle]);
    expect(r.assignments[0]?.nodeId).toBe('idle');
  });

  it('respects host reservations (the dev laptop case)', () => {
    const laptop = node('laptop', {
      cpus: 8,
      memGb: 32,
      settings: {
        maxConcurrency: 4,
        reserved: { cpus: 6, memoryMb: 24 * 1024 },
        labels: [],
        schedulingMode: 'enabled',
      },
    });
    const small = job({ resources: { cpus: 2, memoryMb: 4096 } });
    const big = job({ resources: { cpus: 4, memoryMb: 4096 } });
    const r = schedule([small, big], [laptop]);
    expect(r.assignments.map((a) => a.jobId)).toEqual([small.id]);
    expect(r.pending.get(big.id)).toMatch(/allows at most 2 CPU/);
  });

  it('accounts for allocations made earlier in the same pass', () => {
    const n = node('a', { cpus: 4, memGb: 64 });
    const jobs = [job(), job(), job()];
    const r = schedule(jobs, [n]);
    expect(r.assignments).toHaveLength(2);
    expect(r.pending.get(jobs[2]!.id)).toMatch(/waiting for 2 CPU/);
  });

  it('enforces max concurrency', () => {
    const n = node('a', {
      settings: {
        maxConcurrency: 1,
        reserved: { cpus: 0, memoryMb: 0 },
        labels: [],
        schedulingMode: 'enabled',
      },
    });
    const r = schedule([job(), job()], [n]);
    expect(r.assignments).toHaveLength(1);
    expect([...r.pending.values()][0]).toMatch(/concurrency limit/);
  });

  it('skips disabled and draining nodes', () => {
    const d = node('d', {
      settings: {
        maxConcurrency: 4,
        reserved: { cpus: 0, memoryMb: 0 },
        labels: [],
        schedulingMode: 'draining',
      },
    });
    const j = job();
    const r = schedule([j], [d]);
    expect(r.assignments).toHaveLength(0);
    expect(r.pending.get(j.id)).toBe('node d is draining');
  });

  it('honours label requirements', () => {
    const plain = node('plain');
    const gpu = node('gpu', { effectiveLabels: ['docker', 'gpu'] });
    const j = job({ target: { labels: ['gpu'] } });
    expect(schedule([j], [plain, gpu]).assignments[0]?.nodeId).toBe('gpu');
    const k = job({ target: { labels: ['tpu'] } });
    expect(schedule([k], [plain]).pending.get(k.id)).toMatch(/lacks label\(s\) tpu/);
    expect(schedule([k], [plain, gpu]).pending.get(k.id)).toBe('no eligible node has label(s) tpu');
  });

  it('honours pinning by id or name, and explains a missing node', () => {
    const a = node('a');
    const b = { ...node('b-id'), name: 'b' };
    expect(
      schedule([job({ target: { node: 'b', labels: [] } })], [a, b]).assignments[0]?.nodeId,
    ).toBe('b-id');
    const j = job({ target: { node: 'zzz', labels: [] } });
    expect(schedule([j], [a, b]).pending.get(j.id)).toBe('node "zzz" does not exist');
  });

  it('orders by priority then queue time, with backfill', () => {
    const n = node('a', { cpus: 4, memGb: 64 });
    const low = job({ priority: 0, resources: { cpus: 2, memoryMb: 1024 } });
    const huge = job({ priority: 10, resources: { cpus: 8, memoryMb: 1024 } });
    const high = job({ priority: 5, resources: { cpus: 2, memoryMb: 1024 } });
    const r = schedule([low, huge, high], [n]);
    expect(r.assignments.map((a) => a.jobId)).toEqual([high.id, low.id]);
    expect(r.pending.has(huge.id)).toBe(true);
  });

  it('skips nodes without the needed executor or with Docker unavailable', () => {
    const noDocker = node('nd', {
      info: info(16, 64, { docker: { available: false }, executors: [] }),
    });
    const j = job();
    expect(schedule([j], [noDocker]).pending.get(j.id)).toMatch(/Docker unavailable/);
  });

  it('respects live available memory', () => {
    const n = node('a', {
      metrics: {
        ts: 0,
        cpuPercent: 10,
        memoryAvailableMb: 1024,
        diskFreeMb: 1,
        diskTotalMb: 1,
        load1: 0,
      },
    });
    const j = job({ resources: { cpus: 1, memoryMb: 2048 } });
    expect(schedule([j], [n]).pending.get(j.id)).toMatch(/memory available right now/);
  });

  it('prefers warm nodes for the same project', () => {
    const a = node('a');
    const b = node('b');
    const j = job({ project: 'web' });
    const r = schedule([j], [a, b], { warmNodes: () => new Set(['b']) });
    expect(r.assignments[0]?.nodeId).toBe('b');
  });

  it('skips nodes that rejected the job', () => {
    const j = job({ excludedNodes: new Set(['a']) });
    const r = schedule([j], [node('a'), node('b')]);
    expect(r.assignments[0]?.nodeId).toBe('b');
  });
});

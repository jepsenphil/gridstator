import { statfs } from 'node:fs/promises';
import { availableParallelism, cpus, freemem, hostname, loadavg, release, totalmem } from 'node:os';
import { GRIDSTATOR_VERSION, machineIdentity } from '@gridstator/shared';
import type { NodeInfo, NodeMetrics } from '@gridstator/protocol';
import { probeCommandVersion, type DockerCli } from '@gridstator/executors';

const MB = 1024 * 1024;

export function actBinary(): string {
  return process.env['GRIDSTATOR_ACT'] ?? 'act';
}

export async function collectNodeInfo(
  docker: DockerCli,
  actBin: string = actBinary(),
): Promise<NodeInfo> {
  const [dockerStatus, buildx, act, git] = await Promise.all([
    docker.probe(),
    docker.probeBuildx(),
    probeCommandVersion(actBin, ['--version']),
    probeCommandVersion('git', ['--version']),
  ]);
  const executors: NodeInfo['executors'] = [];
  if (dockerStatus.available) {
    executors.push('command');
    if (buildx.available) executors.push('docker-build');
    if (act.available && git.available) executors.push('actions');
  }
  if (act.available && !git.available)
    act.error = 'git is not installed (required for workflow jobs)';
  return {
    hostname: hostname(),
    machineId: await machineIdentity(),
    platform: process.platform,
    osRelease: release(),
    arch: process.arch,
    cpus: availableParallelism(),
    memoryTotalMb: Math.floor(totalmem() / MB),
    agentVersion: GRIDSTATOR_VERSION,
    docker: dockerStatus,
    buildx,
    act,
    executors,
  };
}

/** Samples CPU utilisation (delta between calls), available memory, disk space and load. */
export class MetricsSampler {
  private previous = cpus().map((c) => c.times);
  private readonly diskPaths: string[];

  constructor(diskPaths: string[]) {
    this.diskPaths = diskPaths;
  }

  private cpuPercent(): number {
    const current = cpus().map((c) => c.times);
    let idle = 0;
    let total = 0;
    current.forEach((t, i) => {
      const p = this.previous[i] ?? t;
      const dIdle = t.idle - p.idle;
      const dTotal =
        t.user + t.nice + t.sys + t.irq + t.idle - (p.user + p.nice + p.sys + p.irq + p.idle);
      idle += dIdle;
      total += dTotal;
    });
    this.previous = current;
    if (total <= 0) return 0;
    return Math.max(0, Math.min(100, Math.round((1 - idle / total) * 1000) / 10));
  }

  private async disk(): Promise<{ free: number; total: number }> {
    for (const path of this.diskPaths) {
      try {
        const s = await statfs(path);
        return {
          free: Math.floor((s.bavail * s.bsize) / MB),
          total: Math.floor((s.blocks * s.bsize) / MB),
        };
      } catch {
        // try the next path
      }
    }
    return { free: 0, total: 0 };
  }

  async sample(): Promise<NodeMetrics> {
    const disk = await this.disk();
    return {
      ts: Date.now(),
      cpuPercent: this.cpuPercent(),
      memoryAvailableMb: Math.floor(freemem() / MB),
      diskFreeMb: disk.free,
      diskTotalMb: disk.total,
      load1: Math.round((loadavg()[0] ?? 0) * 100) / 100,
    };
  }
}

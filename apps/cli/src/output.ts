import { networkInterfaces } from 'node:os';
import { formatBytes, formatDuration } from '@gridstator/shared';
import type { Job, JobState, Node } from '@gridstator/protocol';

const useColor = (stream: NodeJS.WriteStream) =>
  stream.isTTY === true && process.env['NO_COLOR'] === undefined && process.env['TERM'] !== 'dumb';

function wrap(code: string, reset: string) {
  return (text: string, stream: NodeJS.WriteStream = process.stdout) =>
    useColor(stream) ? `\u001b[${code}m${text}\u001b[${reset}m` : text;
}

export const c = {
  bold: wrap('1', '22'),
  dim: wrap('2', '22'),
  red: wrap('31', '39'),
  green: wrap('32', '39'),
  yellow: wrap('33', '39'),
  blue: wrap('34', '39'),
  cyan: wrap('36', '39'),
  gray: wrap('90', '39'),
};

export function stateColor(state: JobState, stream: NodeJS.WriteStream = process.stdout): string {
  switch (state) {
    case 'succeeded':
      return c.green(state, stream);
    case 'failed':
    case 'lost':
      return c.red(state, stream);
    case 'cancelled':
      return c.yellow(state, stream);
    case 'running':
    case 'starting':
      return c.cyan(state, stream);
    default:
      return c.gray(state, stream);
  }
}

// eslint-disable-next-line no-control-regex
const ANSI = /\u001b\[[0-9;]*m/g;
const visibleLength = (s: string) => s.replace(ANSI, '').length;

export function table(headers: string[], rows: string[][]): string {
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => visibleLength(r[i] ?? ''))),
  );
  const line = (cells: string[]) =>
    cells
      .map((cell, i) =>
        i === cells.length - 1 ? cell : cell + ' '.repeat(widths[i]! - visibleLength(cell)),
      )
      .join('  ');
  return [c.bold(line(headers)), ...rows.map(line)].join('\n');
}

export function printJson(value: unknown): void {
  process.stdout.write(JSON.stringify(value, null, 2) + '\n');
}

export function info(message: string): void {
  process.stderr.write(`${message}\n`);
}

export function ago(ts: number | null, now = Date.now()): string {
  if (ts === null) return '-';
  return `${formatDuration(now - ts)} ago`;
}

export function jobDuration(job: Job, now = Date.now()): string {
  if (job.startedAt === null) return '-';
  return formatDuration((job.finishedAt ?? now) - job.startedAt);
}

export function shortId(id: string): string {
  return id;
}

export function nodeRow(n: Node): string[] {
  const status =
    n.status === 'online'
      ? n.health === 'degraded'
        ? c.yellow('degraded')
        : c.green('online')
      : c.gray('offline');
  const mode =
    n.settings.schedulingMode === 'enabled' ? 'enabled' : c.yellow(n.settings.schedulingMode);
  const cpu = n.info
    ? `${n.metrics ? `${n.metrics.cpuPercent.toFixed(0)}% of ` : ''}${n.info.cpus}`
    : '-';
  const mem = n.info
    ? `${n.metrics ? `${formatBytes(n.metrics.memoryAvailableMb * 1024 * 1024)} free / ` : ''}${formatBytes(n.info.memoryTotalMb * 1024 * 1024)}`
    : '-';
  const jobs = `${n.activeJobs.length}/${n.settings.maxConcurrency}`;
  const caps = n.info
    ? [
        n.info.docker.available ? 'docker' : c.red('no-docker'),
        n.info.buildx.available ? 'buildx' : '',
        n.info.act.available ? 'act' : '',
      ]
        .filter(Boolean)
        .join(',')
    : '-';
  return [n.name, status, mode, cpu, mem, jobs, caps, n.settings.labels.join(',') || '-'];
}

export const NODE_HEADERS = [
  'NAME',
  'STATUS',
  'SCHEDULING',
  'CPU',
  'MEMORY',
  'JOBS',
  'CAPABILITIES',
  'LABELS',
];

export function jobRow(j: Job, now = Date.now()): string[] {
  const where =
    j.state === 'queued'
      ? c.gray(`#${j.queuePosition ?? '?'} ${j.stateReason ?? ''}`.trim())
      : (j.nodeName ?? '-');
  return [
    j.id,
    stateColor(j.state),
    j.name.length > 48 ? `${j.name.slice(0, 45)}...` : j.name,
    where,
    jobDuration(j, now),
    ago(j.createdAt, now),
  ];
}

export const JOB_HEADERS = ['ID', 'STATE', 'NAME', 'NODE', 'DURATION', 'CREATED'];

/** Best guess at this machine's LAN address, for printing connection instructions. */
export function lanAddress(): string | undefined {
  for (const addrs of Object.values(networkInterfaces())) {
    for (const a of addrs ?? []) {
      if (a.family === 'IPv4' && !a.internal) return a.address;
    }
  }
  return undefined;
}

/** CLI exit code for a finished job. */
export function exitCodeForJob(job: Job): number {
  switch (job.state) {
    case 'succeeded':
      return 0;
    case 'failed':
      return job.exitCode !== null && job.exitCode > 0 && job.exitCode < 256 ? job.exitCode : 1;
    case 'cancelled':
      return 130;
    case 'lost':
      return 75;
    default:
      return 1;
  }
}

/** Workers that count against `--check`: reachable and not on the controller's build. */
export function notCurrent(nodes: Node[]): Node[] {
  return nodes.filter(
    (n) => n.update !== undefined && n.update.state !== 'current' && n.update.state !== 'unknown',
  );
}

export function summarizeVersions(nodes: Node[], controllerBuild: string): string {
  const current = nodes.filter((n) => n.update?.state === 'current').length;
  const unknown = nodes.filter((n) => n.update?.state === 'unknown').length;
  const behind = notCurrent(nodes).length;
  const reachable = nodes.length - unknown;
  const parts = [`${current}/${reachable} reachable worker(s) on ${controllerBuild}`];
  if (behind > 0) parts.push(`${behind} not current`);
  if (unknown > 0) parts.push(`${unknown} offline (unknown)`);
  return parts.join(', ');
}

import { writeFile, access } from 'node:fs/promises';
import { basename, join, posix, sep } from 'node:path';
import type { Command } from 'commander';
import {
  GridstatorError,
  defaultCacheDir,
  defaultClientConfigPath,
  formatBytes,
  formatDuration,
  parseBytes,
  parseDuration,
  parseMegabytes,
} from '@gridstator/shared';
import {
  DEFAULT_IMAGE,
  DEFAULT_RESOURCES,
  JOB_STATES,
  isTerminal,
  type Job,
  type JobSpec,
  type JobSpecInput,
  type JobState,
  type LogEntry,
  type ProjectConfig,
  type TaskConfig,
} from '@gridstator/protocol';
import {
  GridstatorClient,
  connect,
  fetchPinnedCa,
  normalizeControllerUrl,
  readClientConfig,
  writeClientConfig,
} from '@gridstator/client';
import {
  DEFAULT_PROJECT_CONFIG_YAML,
  captureSnapshot,
  findProject,
  loadProjectConfig,
  uploadSnapshot,
  type CapturedSnapshot,
} from '@gridstator/snapshot';
import {
  JOB_HEADERS,
  NODE_HEADERS,
  ago,
  c,
  exitCodeForJob,
  info,
  jobDuration,
  jobRow,
  lanAddress,
  nodeRow,
  printJson,
  stateColor,
  table,
} from '../output.js';
import { parseLabels } from '../settings-flags.js';

/** Set by commands that need a specific process exit code. */
export const exitState = { code: 0 };

export async function withClient<T>(fn: (client: GridstatorClient) => Promise<T>): Promise<T> {
  const client = await connect();
  try {
    return await fn(client);
  } finally {
    client.close();
  }
}

export function collect(value: string, previous: string[] = []): string[] {
  return [...previous, value];
}

// ---- snapshots ---------------------------------------------------------------------------------

export async function captureProject(
  cwd: string,
  options: { quiet?: boolean } = {},
): Promise<{
  captured: CapturedSnapshot;
  config: ProjectConfig;
  workdir: string;
  root: string;
}> {
  const loc = await findProject(cwd);
  const config = await loadProjectConfig(loc.configPath);
  const captured = await captureSnapshot({
    root: loc.root,
    include: config.snapshot.include,
    exclude: config.snapshot.exclude,
    allowSecrets: config.snapshot.allowSecrets,
    maxFileSize:
      typeof config.snapshot.maxFileSize === 'number'
        ? config.snapshot.maxFileSize * 1024 * 1024
        : parseBytes(config.snapshot.maxFileSize),
    cacheDir: defaultCacheDir(),
  });
  if (!options.quiet) {
    const s = captured.stats;
    info(
      c.dim(
        `Snapshot of ${loc.root}: ${s.files} files, ${formatBytes(s.totalBytes)} (${s.source}, ${s.hashedFiles} hashed, ${formatDuration(s.durationMs)})`,
        process.stderr,
      ),
    );
    const shown = captured.warnings.slice(0, 10);
    for (const w of shown) info(c.yellow(`  ! ${w.path}: ${w.message}`, process.stderr));
    if (captured.warnings.length > shown.length) {
      info(
        c.yellow(
          `  ! ...and ${captured.warnings.length - shown.length} more (see \`gridstator snapshot --json\`)`,
          process.stderr,
        ),
      );
    }
  }
  return { captured, config, workdir: loc.workdir, root: loc.root };
}

export async function uploadCaptured(
  client: GridstatorClient,
  captured: CapturedSnapshot,
  quiet: boolean,
): Promise<void> {
  const { progress } = await uploadSnapshot(captured, client, (p) => {
    if (!quiet && p.missing > 0 && p.uploadedBlobs === 0) {
      info(
        c.dim(
          `Uploading ${p.missing} new blob(s), ${formatBytes(p.missingBytes)}...`,
          process.stderr,
        ),
      );
    }
  });
  if (!quiet && progress.missing === 0)
    info(c.dim('Snapshot already on the controller (nothing to upload)', process.stderr));
}

// ---- job building ------------------------------------------------------------------------------

interface RunFlags {
  artifact: string[];
  image?: string;
  node?: string;
  label: string[];
  cpus?: string;
  memory?: string;
  env: string[];
  secretEnv: string[];
  timeout?: string;
  priority?: string;
  retries?: string;
  snapshot: boolean;
  detach?: boolean;
  json?: boolean;
  task?: string;
  name?: string;
  shell?: boolean;
}

export function parseEnvPairs(pairs: string[]): Record<string, string> {
  const env: Record<string, string> = {};
  for (const pair of pairs) {
    const i = pair.indexOf('=');
    if (i <= 0) {
      if (process.env[pair] === undefined)
        throw new GridstatorError(
          'BAD_REQUEST',
          `--env ${pair}: expected NAME=value (or a variable set in this shell)`,
        );
      env[pair] = process.env[pair]!;
    } else {
      env[pair.slice(0, i)] = pair.slice(i + 1);
    }
  }
  return env;
}

export function secretEnvFrom(names: string[]): Record<string, string> {
  const env: Record<string, string> = {};
  for (const name of names) {
    const value = process.env[name];
    if (value === undefined)
      throw new GridstatorError(
        'BAD_REQUEST',
        `Secret ${name} is not set in this shell's environment`,
      );
    env[name] = value;
  }
  return env;
}

/** Joins `base` and `p` (both relative to the project root) and rejects paths that escape it. */
export function normalizeRel(base: string, p: string): string {
  const joined = posix.normalize(posix.join(base, p.split(sep).join('/')));
  if (joined === '..' || joined.startsWith('../') || posix.isAbsolute(joined)) {
    throw new GridstatorError('BAD_REQUEST', `Path "${p}" is outside the project`);
  }
  return joined === '' ? '.' : joined.replace(/\/$/, '');
}

function toCommand(cmd: string | string[], forceShell: boolean): string[] {
  if (typeof cmd === 'string') return ['sh', '-c', cmd];
  if (forceShell || cmd.length === 1) return ['sh', '-c', cmd.join(' ')];
  return cmd;
}

export function buildSpec(
  args: string[],
  flags: RunFlags,
  config: ProjectConfig,
  extras: { snapshot?: string; workdir: string; project: string },
): JobSpecInput {
  let task: TaskConfig | undefined;
  if (flags.task) {
    task = config.tasks[flags.task];
    if (!task) {
      const known = Object.keys(config.tasks);
      throw new GridstatorError('BAD_REQUEST', `No task named "${flags.task}" in gridstator.yml`, {
        hint: known.length
          ? `Known tasks: ${known.join(', ')}`
          : 'Define tasks under `tasks:` in gridstator.yml.',
      });
    }
  }
  const commandSource: string | string[] | undefined = args.length > 0 ? args : task?.command;
  if (!commandSource)
    throw new GridstatorError('BAD_REQUEST', 'Nothing to run', {
      hint: 'Usage: gridstator run [options] -- <command...>  or  gridstator run --task <name>',
    });
  const resources = { ...config.defaults.resources, ...task?.resources };
  const memory = flags.memory ?? resources.memory;
  const timeout = flags.timeout ?? task?.timeout ?? config.defaults.timeout;
  const labels = [
    ...(config.defaults.labels ?? []),
    ...(task?.labels ?? []),
    ...flags.label.flatMap(parseLabels),
  ];
  const node = flags.node ?? task?.node;
  const workdir = task?.workdir ? task.workdir.replace(/^\.\/?/, '') || '.' : extras.workdir;
  // Task paths are relative to the project root; --artifact paths are relative to the current directory.
  const artifacts = [
    ...(task?.artifacts ?? []).map((p) => normalizeRel('.', p)),
    ...(task?.junit ? [normalizeRel('.', task.junit)] : []),
    ...flags.artifact.map((p) => normalizeRel(extras.workdir, p)),
  ];
  return {
    kind: 'command',
    artifacts: [...new Set(artifacts)],
    ...(flags.name ? { name: flags.name } : flags.task ? { name: `task:${flags.task}` } : {}),
    image: flags.image ?? task?.image ?? config.defaults.image ?? DEFAULT_IMAGE,
    command: toCommand(commandSource, flags.shell ?? false),
    env: { ...task?.env, ...parseEnvPairs(flags.env) },
    secretEnv: secretEnvFrom([...(task?.secretEnv ?? []), ...flags.secretEnv]),
    ...(extras.snapshot ? { snapshot: extras.snapshot } : {}),
    workdir,
    resources: {
      cpus:
        flags.cpus !== undefined ? Number(flags.cpus) : (resources.cpus ?? DEFAULT_RESOURCES.cpus),
      memoryMb:
        memory === undefined
          ? DEFAULT_RESOURCES.memoryMb
          : typeof memory === 'number'
            ? memory
            : parseMegabytes(memory),
    },
    target: { labels: [...new Set(labels)], ...(node ? { node } : {}) },
    ...(timeout ? { timeoutSeconds: Math.ceil(parseDuration(timeout) / 1000) } : {}),
    retry: {
      max: flags.retries !== undefined ? Number(flags.retries) : (task?.retry?.max ?? 0),
      on: task?.retry?.on ?? ['lost'],
    },
    priority: flags.priority !== undefined ? Number(flags.priority) : (task?.priority ?? 0),
    project: extras.project,
  };
}

// ---- log following -----------------------------------------------------------------------------

function writeEntries(entries: LogEntry[]): void {
  for (const e of entries) {
    if (e.o === 'stdout') process.stdout.write(e.d);
    else if (e.o === 'stderr') process.stderr.write(e.d);
    else {
      // System entries may span several lines (e.g. docker pull output); prefix each one.
      const lines = e.d.replace(/\n$/, '').split('\n');
      for (const line of lines)
        process.stderr.write(c.dim(`[gridstator] ${line}`, process.stderr) + '\n');
    }
  }
}

/** Streams logs until the job ends. First Ctrl-C cancels the job, a second one detaches. */
export async function followJob(
  client: GridstatorClient,
  job: Job,
  options: { cancelOnInterrupt: boolean },
): Promise<Job> {
  const abort = new AbortController();
  let interrupts = 0;
  const onSigint = () => {
    interrupts++;
    if (interrupts === 1 && options.cancelOnInterrupt) {
      info(
        c.yellow(
          `\nCancelling job ${job.id}... (press Ctrl-C again to detach and leave it running)`,
          process.stderr,
        ),
      );
      client
        .cancelJob(job.id)
        .catch((err: unknown) =>
          info(c.red(`Cancel failed: ${(err as Error).message}`, process.stderr)),
        );
    } else {
      info(
        c.yellow(
          `\nDetached. Follow again with: gridstator logs ${job.id} --follow`,
          process.stderr,
        ),
      );
      abort.abort();
    }
  };
  process.on('SIGINT', onSigint);
  try {
    await client.followLogs(job.id, writeEntries, {
      signal: abort.signal,
      onReconnect: () =>
        info(c.dim('[gridstator] connection to controller lost; reconnecting...', process.stderr)),
    });
  } catch (err) {
    if (abort.signal.aborted) {
      exitState.code = 0;
      return client.getJob(job.id);
    }
    throw err;
  } finally {
    process.off('SIGINT', onSigint);
  }
  return client.getJob(job.id);
}

function specLines(s: JobSpec): string[] {
  switch (s.kind) {
    case 'command':
      return [
        `  image       ${s.image}`,
        `  command     ${JSON.stringify(s.command)}`,
        `  workdir     ${s.workdir}`,
      ];
    case 'docker-build':
      return [
        `  dockerfile  ${s.dockerfile ?? (s.context === '.' ? 'Dockerfile' : `${s.context}/Dockerfile`)}${s.stage ? ` (target ${s.stage})` : ''}`,
        `  context     ${s.context}`,
        `  output      ${s.output}${s.tags.length ? ` → ${s.tags.join(', ')}` : ''}`,
      ];
    case 'actions':
      return [
        `  workflow    ${s.workflow}${s.job ? ` (job ${s.job})` : ''}`,
        `  event       ${s.event}`,
        ...(Object.keys(s.inputs).length
          ? [
              `  inputs      ${Object.entries(s.inputs)
                .map(([k, v]) => `${k}=${v}`)
                .join(', ')}`,
            ]
          : []),
      ];
  }
}

export function summarize(job: Job): void {
  const parts = [`Job ${job.id} ${stateColor(job.state, process.stderr)}`];
  if (job.nodeName) parts.push(`on ${job.nodeName}`);
  if (job.startedAt) parts.push(`in ${jobDuration(job)}`);
  if (job.state === 'failed' && job.exitCode !== null) parts.push(`(exit code ${job.exitCode})`);
  info(parts.join(' '));
  if (job.error && job.state !== 'succeeded') info(c.red(`  ${job.error}`, process.stderr));
  if (job.state === 'lost') info(c.dim(`  Retry with: gridstator retry ${job.id}`, process.stderr));
}

// ---- command registration ----------------------------------------------------------------------

export function registerClientCommands(program: Command): void {
  program
    .command('login')
    .description('Connect this machine to a controller (verifies the CA fingerprint and token)')
    .argument('<url>', 'controller URL, e.g. https://192.168.1.20:7300')
    .requiredOption(
      '--fingerprint <sha256>',
      'cluster CA fingerprint (printed by `controller init`)',
    )
    .requiredOption('--token <token>', 'client token (from `gridstator token create`)')
    .action(async (url: string, opts: { fingerprint: string; token: string }) => {
      const base = normalizeControllerUrl(url);
      const ca = await fetchPinnedCa(base, opts.fingerprint);
      const client = new GridstatorClient({ url: base, ca, token: opts.token });
      try {
        const cluster = await client.cluster();
        await writeClientConfig({
          version: 1,
          url: base,
          caPem: ca,
          caFingerprint: cluster.caFingerprint,
          token: opts.token,
          tokenName: cluster.whoami.name,
        });
        info(
          c.green(
            `Logged in to ${base} as "${cluster.whoami.name}" (${cluster.whoami.role}).`,
            process.stderr,
          ),
        );
        info(
          `${cluster.nodes.online}/${cluster.nodes.total} node(s) online. Saved to ${defaultClientConfigPath()}.`,
        );
      } finally {
        client.close();
      }
    });

  program
    .command('status')
    .description('Show cluster, node and job summary')
    .option('--json', 'machine-readable output')
    .action(async (opts: { json?: boolean }) =>
      withClient(async (client) => {
        const [cluster, nodes, active] = await Promise.all([
          client.cluster(),
          client.listNodes(),
          client.listJobs({ state: ['queued', 'assigned', 'starting', 'running'], limit: 100 }),
        ]);
        if (opts.json) return printJson({ cluster, nodes, activeJobs: active });
        process.stdout.write(
          `${c.bold('Controller')} ${client.url}  v${cluster.version}  cluster ${cluster.clusterId}  (signed in as ${cluster.whoami.name})\n`,
        );
        process.stdout.write(
          `${c.bold('Nodes')} ${cluster.nodes.online}/${cluster.nodes.total} online\n\n`,
        );
        if (nodes.length > 0)
          process.stdout.write(table(NODE_HEADERS, nodes.map(nodeRow)) + '\n\n');
        const counts = JOB_STATES.filter((s) => (cluster.jobs[s] ?? 0) > 0).map(
          (s) => `${s} ${cluster.jobs[s]}`,
        );
        process.stdout.write(`${c.bold('Jobs')} ${counts.join(', ') || 'none yet'}\n`);
        if (active.length > 0)
          process.stdout.write(
            '\n' +
              table(
                JOB_HEADERS,
                active.map((j) => jobRow(j)),
              ) +
              '\n',
          );
      }),
    );

  const nodes = program
    .command('nodes')
    .description('List and manage worker nodes')
    .option('--json', 'machine-readable output');
  nodes.action(async (opts: { json?: boolean }) =>
    withClient(async (client) => {
      const list = await client.listNodes();
      if (opts.json) return printJson(list);
      if (list.length === 0) {
        info(
          'No nodes yet. Create a join token with `gridstator token create --join` and run `gridstator agent join` on a worker.',
        );
        return;
      }
      process.stdout.write(table(NODE_HEADERS, list.map(nodeRow)) + '\n');
      for (const n of list)
        for (const issue of n.healthIssues)
          info(c.yellow(`  ! ${n.name}: ${issue}`, process.stderr));
    }),
  );

  nodes
    .command('show')
    .argument('<node>', 'node name or id')
    .option('--json', 'machine-readable output')
    .action(async (name: string, opts: { json?: boolean }) =>
      withClient(async (client) => {
        const n = await client.getNode(name);
        if (opts.json) return printJson(n);
        const lines = [
          `${c.bold(n.name)}  ${n.id}`,
          `  status        ${n.status}${n.health === 'degraded' ? ` (degraded: ${n.healthIssues.join('; ')})` : ''}`,
          `  scheduling    ${n.settings.schedulingMode}, max ${n.settings.maxConcurrency} concurrent job(s)`,
          `  reserved      ${n.settings.reserved.cpus} CPU, ${formatBytes(n.settings.reserved.memoryMb * 1024 * 1024)} for the host`,
          `  allocatable   ${n.allocatable.cpus} CPU, ${formatBytes(n.allocatable.memoryMb * 1024 * 1024)}`,
          `  allocated     ${n.allocated.cpus} CPU, ${formatBytes(n.allocated.memoryMb * 1024 * 1024)} (${n.activeJobs.length} job(s))`,
          `  labels        ${n.effectiveLabels.join(', ') || '-'}`,
          `  last seen     ${ago(n.lastSeenAt)}`,
        ];
        if (n.info) {
          lines.push(
            `  system        ${n.info.platform}/${n.info.arch} ${n.info.osRelease}, ${n.info.cpus} CPU, ${formatBytes(n.info.memoryTotalMb * 1024 * 1024)}, host ${n.info.hostname}`,
            `  docker        ${n.info.docker.available ? n.info.docker.version : `unavailable (${n.info.docker.error})`}`,
            `  buildx        ${n.info.buildx.available ? (n.info.buildx.version ?? 'yes') : 'unavailable'}`,
            `  act           ${n.info.act.available ? (n.info.act.version ?? 'yes') : 'unavailable'}`,
            `  agent         v${n.info.agentVersion}`,
          );
        }
        if (n.metrics) {
          lines.push(
            `  cpu           ${n.metrics.cpuPercent}% (load ${n.metrics.load1})`,
            `  memory free   ${formatBytes(n.metrics.memoryAvailableMb * 1024 * 1024)}`,
            `  disk free     ${formatBytes(n.metrics.diskFreeMb * 1024 * 1024)} of ${formatBytes(n.metrics.diskTotalMb * 1024 * 1024)}`,
          );
        }
        if (n.activeJobs.length > 0) lines.push(`  active jobs   ${n.activeJobs.join(', ')}`);
        process.stdout.write(lines.join('\n') + '\n');
      }),
    );

  nodes
    .command('set')
    .description('Change scheduling settings for a node')
    .argument('<node>', 'node name or id')
    .option('--max-concurrency <n>', 'maximum concurrent jobs')
    .option('--reserve-cpus <n>', 'CPUs held back for the host')
    .option('--reserve-memory <size>', 'memory held back for the host, e.g. 8g')
    .option('--labels <list>', 'replace labels (comma-separated; empty string clears)')
    .action(
      async (
        name: string,
        opts: {
          maxConcurrency?: string;
          reserveCpus?: string;
          reserveMemory?: string;
          labels?: string;
        },
      ) =>
        withClient(async (client) => {
          const patch = {
            ...(opts.maxConcurrency !== undefined
              ? { maxConcurrency: Number(opts.maxConcurrency) }
              : {}),
            ...(opts.reserveCpus !== undefined || opts.reserveMemory !== undefined
              ? {
                  reserved: {
                    ...(opts.reserveCpus !== undefined ? { cpus: Number(opts.reserveCpus) } : {}),
                    ...(opts.reserveMemory !== undefined
                      ? { memoryMb: parseMegabytes(opts.reserveMemory) }
                      : {}),
                  },
                }
              : {}),
            ...(opts.labels !== undefined
              ? { labels: opts.labels === '' ? [] : parseLabels(opts.labels) }
              : {}),
          };
          if (Object.keys(patch).length === 0)
            throw new GridstatorError('BAD_REQUEST', 'Nothing to change', {
              hint: 'See `gridstator nodes set --help`.',
            });
          const n = await client.updateNode(name, patch);
          info(
            `Updated ${n.name}: max ${n.settings.maxConcurrency} job(s), reserved ${n.settings.reserved.cpus} CPU / ${formatBytes(n.settings.reserved.memoryMb * 1024 * 1024)}, labels ${n.settings.labels.join(',') || '-'}`,
          );
        }),
    );

  for (const [cmd, mode, desc] of [
    ['enable', 'enabled', 'Allow the scheduler to place jobs on a node'],
    ['disable', 'disabled', 'Stop placing new jobs on a node (running jobs continue)'],
    ['drain', 'draining', 'Stop placing new jobs on a node and let running jobs finish'],
  ] as const) {
    nodes
      .command(cmd)
      .description(desc)
      .argument('<node>', 'node name or id')
      .action(async (name: string) =>
        withClient(async (client) => {
          const n = await client.updateNode(name, { schedulingMode: mode });
          info(
            `${n.name} is now ${n.settings.schedulingMode}${mode === 'draining' && n.activeJobs.length > 0 ? ` (${n.activeJobs.length} job(s) still running)` : ''}.`,
          );
        }),
      );
  }

  nodes
    .command('remove')
    .description('Remove a node and revoke its credential (admin)')
    .argument('<node>', 'node name or id')
    .option('--force', 'cancel jobs still running on the node')
    .action(async (name: string, opts: { force?: boolean }) =>
      withClient(async (client) => {
        await client.request('DELETE', `/v1/nodes/${encodeURIComponent(name)}`, {
          query: { force: opts.force ? 'true' : undefined },
        });
        info(`Removed node ${name}.`);
      }),
    );

  program
    .command('jobs')
    .description('List jobs (most recent first)')
    .option('--state <states>', `filter by state (comma-separated: ${JOB_STATES.join(', ')})`)
    .option('--node <node>', 'filter by node')
    .option('--limit <n>', 'maximum number of jobs', '30')
    .option('--json', 'machine-readable output')
    .action(async (opts: { state?: string; node?: string; limit: string; json?: boolean }) =>
      withClient(async (client) => {
        const states = opts.state
          ? (opts.state.split(',').map((s) => s.trim()) as JobState[])
          : undefined;
        const jobs = await client.listJobs({
          ...(states ? { state: states } : {}),
          ...(opts.node ? { node: opts.node } : {}),
          limit: Number(opts.limit),
        });
        if (opts.json) return printJson(jobs);
        if (jobs.length === 0) return info('No jobs.');
        process.stdout.write(
          table(
            JOB_HEADERS,
            jobs.map((j) => jobRow(j)),
          ) + '\n',
        );
      }),
    );

  program
    .command('job')
    .description('Show details of one job')
    .argument('<id>', 'job id')
    .option('--json', 'machine-readable output')
    .action(async (id: string, opts: { json?: boolean }) =>
      withClient(async (client) => {
        const [job, events] = await Promise.all([client.getJob(id), client.jobEvents(id)]);
        if (opts.json) return printJson({ job, events });
        const s = job.spec;
        const lines = [
          `${c.bold(job.id)}  ${stateColor(job.state)}${job.stateReason ? ` (${job.stateReason})` : ''}`,
          `  name        ${job.name}`,
          `  kind        ${job.kind}`,
          `  owner       ${job.owner}`,
          `  node        ${job.nodeName ?? '-'}`,
          ...specLines(s),
          `  snapshot    ${job.snapshotId ?? '-'}`,
          `  resources   ${s.resources.cpus} CPU, ${formatBytes(s.resources.memoryMb * 1024 * 1024)}`,
          `  attempt     ${job.attempt}${job.retryOf ? ` (retry of ${job.retryOf})` : ''}`,
          `  exit code   ${job.exitCode ?? '-'}`,
          `  duration    ${jobDuration(job)}`,
        ];
        if (job.error) lines.push(`  error       ${c.red(job.error)}`);
        const image = job.outputs?.image;
        if (image) {
          lines.push(
            `  image id    ${image.id ?? '-'}${image.sizeBytes !== null ? ` (${formatBytes(image.sizeBytes)})` : ''}`,
          );
          if (image.digest) lines.push(`  digest      ${image.digest}`);
        }
        for (const a of job.artifacts) {
          lines.push(
            `  artifact    ${a.name} (${formatBytes(a.size)})  →  gridstator artifacts ${job.id} --download ${a.name}`,
          );
        }
        const steps = job.outputs?.steps ?? [];
        if (steps.length > 0) {
          lines.push('', c.bold('  Steps'));
          let group: string | null = null;
          for (const st of steps) {
            if (st.group !== group) {
              group = st.group;
              if (group) lines.push(`  ${group}`);
            }
            const icon =
              st.status === 'success'
                ? c.green('✓')
                : st.status === 'failure'
                  ? c.red('✗')
                  : st.status === 'running'
                    ? c.cyan('…')
                    : c.gray('-');
            lines.push(
              `    ${icon} ${st.name}${st.durationMs !== null ? c.dim(` ${formatDuration(st.durationMs)}`) : ''}`,
            );
          }
        }
        if (Object.keys(s.env).length > 0)
          lines.push(`  env         ${Object.keys(s.env).join(', ')}`);
        if (Object.keys(s.secretEnv).length > 0)
          lines.push(`  secrets     ${Object.keys(s.secretEnv).join(', ')}`);
        lines.push('', c.bold('  History'));
        for (const e of events) {
          const what = e.toState ? `${e.fromState ?? '·'} → ${e.toState}` : e.type;
          lines.push(
            `  ${new Date(e.ts).toISOString()}  ${what.padEnd(24)} ${e.actor}${e.message ? `: ${e.message}` : ''}`,
          );
        }
        process.stdout.write(lines.join('\n') + '\n');
      }),
    );

  program
    .command('run')
    .description(
      'Run a command in a container on a cluster node, against a snapshot of this project',
    )
    .argument('[command...]', 'command to run (after --). A single argument is run with sh -c')
    .option('-t, --task <name>', 'run a task defined in gridstator.yml')
    .option(
      '--image <image>',
      `container image (default from gridstator.yml, else ${DEFAULT_IMAGE})`,
    )
    .option('--node <node>', 'run on this node (name or id)')
    .option('-l, --label <label>', 'require a node label (repeatable)', collect, [])
    .option('--cpus <n>', 'CPUs to reserve and limit the container to')
    .option('--memory <size>', 'memory to reserve and limit the container to, e.g. 4g')
    .option('-e, --env <NAME=value>', 'environment variable (repeatable)', collect, [])
    .option(
      '-s, --secret-env <NAME>',
      'forward a secret from this shell; masked in logs (repeatable)',
      collect,
      [],
    )
    .option('--timeout <duration>', 'stop the job after this long, e.g. 30m')
    .option('--priority <n>', 'higher runs first (-100..100)')
    .option('--retries <n>', 'automatic retries if the node is lost')
    .option('--name <name>', 'display name for the job')
    .option('--shell', 'run the arguments as one shell command (sh -c)')
    .option(
      '-a, --artifact <path>',
      'collect a file or directory as an artifact after the run (repeatable)',
      collect,
      [],
    )
    .option('--no-snapshot', 'do not send the project; run in an empty /workspace')
    .option('-d, --detach', 'print the job id and exit instead of streaming logs')
    .option('--json', 'print the submitted job as JSON (implies --detach)')
    .action(async (args: string[], flags: RunFlags) =>
      withClient(async (client) => {
        const quiet = flags.json ?? false;
        let snapshotId: string | undefined;
        let workdir = '.';
        let config: ProjectConfig;
        let project: string;
        if (flags.snapshot) {
          const cap = await captureProject(process.cwd(), { quiet });
          await uploadCaptured(client, cap.captured, quiet);
          snapshotId = cap.captured.id;
          workdir = cap.workdir;
          config = cap.config;
          project = config.project ?? basename(cap.root);
        } else {
          const loc = await findProject(process.cwd());
          config = await loadProjectConfig(loc.configPath);
          project = config.project ?? basename(loc.root);
        }
        const spec = buildSpec(args, flags, config, {
          ...(snapshotId ? { snapshot: snapshotId } : {}),
          workdir,
          project,
        });
        const job = await client.submitJob(spec);
        if (flags.json) return printJson(job);
        if (flags.detach) {
          process.stdout.write(job.id + '\n');
          return;
        }
        info(
          c.dim(
            `Job ${job.id} submitted${snapshotId ? ` (snapshot ${snapshotId.slice(7, 19)})` : ''}; ${job.stateReason ?? job.state}`,
            process.stderr,
          ),
        );
        const final = await followJob(client, job, { cancelOnInterrupt: true });
        if (isTerminal(final.state)) {
          summarize(final);
          exitState.code = exitCodeForJob(final);
        }
      }),
    );

  program
    .command('logs')
    .description("Print a job's logs")
    .argument('<id>', 'job id')
    .option('-f, --follow', 'keep streaming until the job finishes')
    .option('--json', 'print log entries as JSON lines')
    .action(async (id: string, opts: { follow?: boolean; json?: boolean }) =>
      withClient(async (client) => {
        const job = await client.getJob(id);
        if (opts.follow && !isTerminal(job.state)) {
          if (opts.json) {
            await client.followLogs(id, (entries) =>
              entries.forEach((e) => process.stdout.write(JSON.stringify(e) + '\n')),
            );
          } else {
            const final = await followJob(client, job, { cancelOnInterrupt: false });
            if (isTerminal(final.state)) {
              summarize(final);
              exitState.code = exitCodeForJob(final);
            }
          }
          return;
        }
        let after = 0;
        for (;;) {
          const page = await client.jobLogs(id, after, 10_000);
          if (opts.json)
            page.entries.forEach((e) => process.stdout.write(JSON.stringify(e) + '\n'));
          else writeEntries(page.entries);
          if (page.entries.length < 10_000) break;
          after = page.entries[page.entries.length - 1]!.s;
        }
      }),
    );

  program
    .command('cancel')
    .description('Cancel a queued or running job')
    .argument('<id>', 'job id')
    .action(async (id: string) =>
      withClient(async (client) => {
        const job = await client.cancelJob(id);
        info(
          isTerminal(job.state)
            ? `Job ${id} ${job.state}.`
            : `Cancellation of ${id} requested; the node is stopping it.`,
        );
      }),
    );

  program
    .command('retry')
    .description('Run a finished job again with the same snapshot and settings')
    .argument('<id>', 'job id')
    .option('-d, --detach', 'print the new job id and exit')
    .action(async (id: string, opts: { detach?: boolean }) =>
      withClient(async (client) => {
        const job = await client.retryJob(id);
        if (opts.detach) {
          process.stdout.write(job.id + '\n');
          return;
        }
        info(c.dim(`Retrying ${id} as ${job.id} (attempt ${job.attempt})`, process.stderr));
        const final = await followJob(client, job, { cancelOnInterrupt: true });
        if (isTerminal(final.state)) {
          summarize(final);
          exitState.code = exitCodeForJob(final);
        }
      }),
    );

  program
    .command('snapshot')
    .description('Capture this project as a snapshot and upload it (shows what would be sent)')
    .option('--dry-run', 'capture only; do not upload')
    .option('--json', 'machine-readable output including the full file list')
    .action(async (opts: { dryRun?: boolean; json?: boolean }) => {
      const cap = await captureProject(process.cwd(), { quiet: opts.json ?? false });
      if (!opts.dryRun)
        await withClient((client) => uploadCaptured(client, cap.captured, opts.json ?? false));
      if (opts.json) {
        return printJson({
          id: cap.captured.id,
          root: cap.root,
          workdir: cap.workdir,
          stats: cap.captured.stats,
          warnings: cap.captured.warnings,
          entries: cap.captured.manifest.entries,
          uploaded: !opts.dryRun,
        });
      }
      process.stdout.write(cap.captured.id + '\n');
    });

  const token = program.command('token').description('Manage client and join tokens (admin)');
  token
    .command('create')
    .description('Create a client token, or a join token for adding a worker')
    .option('--join', 'create a join token for `gridstator agent join`')
    .option('--name <name>', 'token name (shown in job ownership and audit records)')
    .option('--role <role>', 'client token role: operator or admin', 'operator')
    .option(
      '--ttl <duration>',
      'expiry, e.g. 24h (join tokens default to 24h; client tokens never expire)',
    )
    .option('--uses <n>', 'join token: number of agents that may use it', '1')
    .option('--json', 'machine-readable output')
    .action(
      async (opts: {
        join?: boolean;
        name?: string;
        role: string;
        ttl?: string;
        uses: string;
        json?: boolean;
      }) =>
        withClient(async (client) => {
          const saved = await readClientConfig();
          const res = await client.createToken({
            kind: opts.join ? 'join' : 'client',
            name:
              opts.name ?? (opts.join ? `join-${new Date().toISOString().slice(0, 10)}` : 'client'),
            role: opts.role === 'admin' ? 'admin' : 'operator',
            ...(opts.ttl ? { ttlSeconds: Math.ceil(parseDuration(opts.ttl) / 1000) } : {}),
            ...(opts.join ? { maxUses: Number(opts.uses) } : {}),
          });
          if (opts.json) return printJson(res);
          const fingerprint = saved?.caFingerprint ?? (await client.cluster()).caFingerprint;
          const url = client.url.replace(
            /127\.0\.0\.1|localhost/,
            lanAddress() ?? '<controller-host>',
          );
          process.stdout.write(res.token + '\n');
          if (opts.join) {
            info(
              `\nOn the worker machine, run:\n\n  gridstator agent join ${url} --token ${res.token} --fingerprint ${fingerprint}\n`,
            );
            info(
              c.dim(
                `Expires ${res.info.expiresAt ? new Date(res.info.expiresAt).toLocaleString() : 'never'}; usable ${res.info.usesRemaining} time(s).`,
                process.stderr,
              ),
            );
          } else {
            info(
              `\nOn the client machine, run:\n\n  gridstator login ${url} --fingerprint ${fingerprint} --token ${res.token}\n`,
            );
          }
        }),
    );

  token
    .command('list')
    .option('--json', 'machine-readable output')
    .action(async (opts: { json?: boolean }) =>
      withClient(async (client) => {
        const tokens = await client.listTokens();
        if (opts.json) return printJson(tokens);
        process.stdout.write(
          table(
            ['ID', 'KIND', 'NAME', 'ROLE', 'CREATED', 'LAST USED', 'STATUS'],
            tokens.map((t) => [
              t.id,
              t.kind,
              t.name,
              t.role ?? '-',
              ago(t.createdAt),
              ago(t.lastUsedAt),
              t.revokedAt
                ? c.red('revoked')
                : t.expiresAt && t.expiresAt < Date.now()
                  ? c.yellow('expired')
                  : t.kind === 'join' && (t.usesRemaining ?? 0) <= 0
                    ? c.gray('used')
                    : c.green('active'),
            ]),
          ) + '\n',
        );
      }),
    );

  token
    .command('revoke')
    .argument('<id>', 'token id (from `gridstator token list`)')
    .action(async (id: string) =>
      withClient(async (client) => {
        await client.revokeToken(id);
        info(`Revoked token ${id}.`);
      }),
    );

  program
    .command('init')
    .description('Create a gridstator.yml in the current project')
    .option('--force', 'overwrite an existing gridstator.yml')
    .action(async (opts: { force?: boolean }) => {
      const loc = await findProject(process.cwd());
      const target = loc.configPath ?? join(loc.root, 'gridstator.yml');
      const exists = await access(target).then(
        () => true,
        () => false,
      );
      if (exists && !opts.force)
        throw new GridstatorError('CONFLICT', `${target} already exists`, {
          hint: 'Pass --force to overwrite it.',
        });
      await writeFile(target, DEFAULT_PROJECT_CONFIG_YAML);
      info(`Wrote ${target}. Try: gridstator run --task test`);
    });
}

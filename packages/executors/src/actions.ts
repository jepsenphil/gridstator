import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdir, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { join, relative, sep } from 'node:path';
import { promisify } from 'node:util';
import type { ActionsJobSpec, JobOutputs } from '@gridstator/protocol';
import { ActLogParser, LineSplitter } from './act-log.js';
import { repoSlug, rewriteSnapshotCheckouts } from './checkout-rewrite.js';
import { cleanupJobResources } from './container.js';
import { type DockerCli, LABEL_JOB, LABEL_MANAGED, LABEL_NODE } from './docker.js';
import {
  cancelled,
  failed,
  resolveInside,
  type ExecutionContext,
  type ExecutionResult,
  type Executor,
} from './executor.js';
import { runProcess } from './process.js';

const execFileAsync = promisify(execFile);

/**
 * Default runner images for common `runs-on` labels (act's "medium" images, which include the
 * tools most workflows expect). Override per project in gridstator.yml (`actions.platforms`).
 */
export const DEFAULT_ACT_PLATFORMS: Readonly<Record<string, string>> = {
  'ubuntu-latest': 'catthehacker/ubuntu:act-latest',
  'ubuntu-24.04': 'catthehacker/ubuntu:act-24.04',
  'ubuntu-22.04': 'catthehacker/ubuntu:act-22.04',
};

export interface ActionsExecutorOptions {
  docker: DockerCli;
  /** Path to the act binary. */
  actBin: string;
  /** Private HOME for act: action clones, tool cache and cache-server data persist here. */
  actHome: string;
  /** Private directory (mode 0700) for short-lived secret files. */
  tmpDir: string;
  stopGraceSeconds?: number;
}

/** Encodes a dotenv value so multi-line secrets survive act's dotenv parser. */
export function dotenvLine(name: string, value: string): string {
  return `${name}=${JSON.stringify(value)}`;
}

/** Builds the act command line (exported for tests). */
export function actArgs(
  spec: ActionsJobSpec,
  paths: {
    src: string;
    workflow: string;
    secretFile: string;
    varFile: string;
    envFile: string;
    artifactDir: string;
    artifactPort: number;
    cacheDir: string;
    eventFile?: string;
  },
  ids: { jobId: string; nodeId: string },
  defaultPlatforms: Readonly<Record<string, string>> = DEFAULT_ACT_PLATFORMS,
): string[] {
  const labels = [
    `${LABEL_MANAGED}=true`,
    `${LABEL_JOB}=${ids.jobId}`,
    `${LABEL_NODE}=${ids.nodeId}`,
  ]
    .map((l) => `--label ${l}`)
    .join(' ');
  const args = [
    spec.event,
    '-C',
    paths.src,
    '-W',
    paths.workflow,
    '--json',
    '--rm',
    '--pull=false',
    // Never read .secrets/.vars/.env from the project; always use our private files.
    '--secret-file',
    paths.secretFile,
    '--var-file',
    paths.varFile,
    '--env-file',
    paths.envFile,
    '--artifact-server-path',
    paths.artifactDir,
    // A port per job, so concurrent workflow jobs on one worker do not collide.
    '--artifact-server-port',
    String(paths.artifactPort),
    '--cache-server-path',
    paths.cacheDir,
    // act binds its cache server to the LAN address by default; keep it on loopback.
    '--cache-server-addr',
    '127.0.0.1',
    '--container-options',
    labels,
  ];
  if (spec.job) args.push('-j', spec.job);
  if (!spec.dockerSocket) args.push('--container-daemon-socket', '-');
  for (const [label, image] of Object.entries({ ...defaultPlatforms, ...spec.platforms }))
    args.push('-P', `${label}=${image}`);
  for (const [k, v] of Object.entries(spec.inputs)) args.push('--input', `${k}=${v}`);
  for (const m of spec.matrix) args.push('--matrix', m);
  if (paths.eventFile) args.push('--eventpath', paths.eventFile);
  return args;
}

function freeLoopbackPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      srv.close(() =>
        typeof addr === 'object' && addr ? resolve(addr.port) : reject(new Error('no free port')),
      );
    });
  });
}

async function git(cwd: string, args: string[], env: NodeJS.ProcessEnv): Promise<void> {
  await execFileAsync(
    'git',
    ['-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', ...args],
    { cwd, env, maxBuffer: 64 * 1024 * 1024 },
  );
}

async function listFiles(dir: string): Promise<string[]> {
  const out: string[] = [];
  const walk = async (d: string) => {
    for (const entry of await readdir(d, { withFileTypes: true })) {
      const p = join(d, entry.name);
      if (entry.isDirectory()) await walk(p);
      else if (entry.isFile()) out.push(p);
    }
  };
  if (existsSync(dir)) await walk(dir);
  return out.sort();
}

/**
 * Runs a GitHub Actions workflow (or one job of it) with nektos/act against the job's snapshot.
 * act copies the staged files into its containers and skips `actions/checkout` for the current
 * repository, so the workflow sees exactly the submitted working tree.
 */
export class ActionsExecutor implements Executor {
  readonly kind = 'actions' as const;
  private readonly opts: ActionsExecutorOptions;

  constructor(options: ActionsExecutorOptions) {
    this.opts = options;
  }

  async run(ctx: ExecutionContext): Promise<ExecutionResult> {
    const spec = ctx.spec;
    if (spec.kind !== 'actions') return failed(`ActionsExecutor cannot run ${spec.kind} jobs`);
    if (!ctx.materialize) return failed('actions jobs need a source snapshot');
    if (ctx.signal.aborted) return cancelled();

    const src = join(ctx.workDir, 'src');
    ctx.system('Staging the snapshot for act');
    await ctx.materialize(src);
    const workflow = await resolveInside(src, spec.workflow, 'file');
    if ('error' in workflow) return failed(`Workflow ${workflow.error}`);

    // Point same-repository checkouts at the snapshot (see checkout-rewrite.ts). Done before the
    // snapshot commit so the job's working tree stays clean.
    for (const r of await rewriteSnapshotCheckouts(src, repoSlug(spec.git?.remoteUrl))) {
      const what = Object.entries(r.removed)
        .map(([k, v]) => `${k}: ${v}`)
        .join(', ');
      ctx.system(
        `${r.file}: job ${r.job}, step "${r.step}" checks out the snapshot (ignoring ${what})`,
      );
    }

    // act derives github.sha/ref/repository from git; give it a repository that mirrors the
    // developer's branch and remote, with one commit containing exactly the snapshot.
    const gitEnv = {
      ...process.env,
      HOME: this.opts.actHome,
      GIT_CONFIG_NOSYSTEM: '1',
      GIT_AUTHOR_NAME: 'Gridstator',
      GIT_AUTHOR_EMAIL: 'gridstator@localhost',
      GIT_COMMITTER_NAME: 'Gridstator',
      GIT_COMMITTER_EMAIL: 'gridstator@localhost',
    };
    try {
      await git(src, ['init', '-q', '-b', spec.git?.branch ?? 'main'], gitEnv);
      await git(src, ['add', '-A'], gitEnv);
      await git(
        src,
        ['commit', '-q', '--allow-empty', '-m', `Gridstator snapshot for job ${ctx.jobId}`],
        gitEnv,
      );
      if (spec.git?.remoteUrl)
        await git(src, ['remote', 'add', 'origin', spec.git.remoteUrl], gitEnv);
    } catch (err) {
      return failed(`Could not prepare the git context for act: ${(err as Error).message}`);
    }

    await mkdir(this.opts.actHome, { recursive: true, mode: 0o700 });
    const secretFile = join(this.opts.tmpDir, `${ctx.jobId}.secrets`);
    const varFile = join(this.opts.tmpDir, `${ctx.jobId}.vars`);
    const envFile = join(this.opts.tmpDir, `${ctx.jobId}.env`);
    const artifactDir = join(ctx.workDir, 'artifacts');
    let eventFile: string | undefined;
    await writeFile(
      secretFile,
      Object.entries(spec.secretEnv)
        .map(([k, v]) => dotenvLine(k, v))
        .join('\n') + '\n',
      { mode: 0o600 },
    );
    await writeFile(
      varFile,
      Object.entries(spec.vars)
        .map(([k, v]) => dotenvLine(k, v))
        .join('\n') + '\n',
      { mode: 0o600 },
    );
    await writeFile(
      envFile,
      Object.entries(spec.env)
        .map(([k, v]) => dotenvLine(k, v))
        .join('\n') + '\n',
      { mode: 0o600 },
    );
    if (spec.eventPayload) {
      eventFile = join(ctx.workDir, 'event.json');
      await writeFile(eventFile, JSON.stringify(spec.eventPayload), { mode: 0o600 });
    }

    const parser = new ActLogParser();
    let lastReport = 0;
    let reportTimer: NodeJS.Timeout | undefined;
    const report = (force = false) => {
      if (!parser.takeChanged() && !force) return;
      const now = Date.now();
      if (!force && now - lastReport < 500) {
        reportTimer ??= setTimeout(() => {
          reportTimer = undefined;
          lastReport = Date.now();
          ctx.reportOutputs({ steps: parser.getSteps() });
        }, 500);
        return;
      }
      lastReport = now;
      ctx.reportOutputs({ steps: parser.getSteps() });
    };
    const handleLine = (origin: 'stdout' | 'stderr') => (line: string) => {
      for (const out of parser.feed(line, origin)) {
        if (out.stream === 'system') ctx.system(out.text);
        else ctx.output(out.stream, Buffer.from(out.text));
      }
      report();
    };
    const outSplit = new LineSplitter();
    const errSplit = new LineSplitter();

    const args = actArgs(
      spec,
      {
        src,
        workflow: workflow.path,
        secretFile,
        varFile,
        envFile,
        artifactDir,
        artifactPort: await freeLoopbackPort(),
        cacheDir: join(this.opts.actHome, 'cache-server'),
        ...(eventFile ? { eventFile } : {}),
      },
      { jobId: ctx.jobId, nodeId: ctx.nodeId },
    );
    const realDockerConfig = process.env['DOCKER_CONFIG'] ?? join(homedir(), '.docker');
    ctx.system(
      `act ${spec.event} -W ${spec.workflow}${spec.job ? ` -j ${spec.job}` : ''} on ${ctx.nodeName}${spec.dockerSocket ? ' (Docker socket mounted)' : ''}`,
    );
    let outcome;
    let stoppedAt: number | undefined;
    ctx.signal.addEventListener('abort', () => (stoppedAt ??= Date.now()), { once: true });
    try {
      outcome = await runProcess(ctx, {
        bin: this.opts.actBin,
        args,
        cwd: src,
        env: {
          ...process.env,
          HOME: this.opts.actHome,
          ...(existsSync(realDockerConfig) ? { DOCKER_CONFIG: realDockerConfig } : {}),
        },
        timeoutSeconds: spec.timeoutSeconds,
        graceSeconds: this.opts.stopGraceSeconds ?? 15,
        onStdout: (c) => outSplit.push(c, handleLine('stdout')),
        onStderr: (c) => errSplit.push(c, handleLine('stderr')),
      });
    } finally {
      outSplit.end(handleLine('stdout'));
      errSplit.end(handleLine('stderr'));
      if (reportTimer) clearTimeout(reportTimer);
      await Promise.all([secretFile, varFile, envFile].map((f) => rm(f, { force: true })));
      // act removes its containers itself; this catches anything left by a crash or SIGKILL.
      const removed = await cleanupJobResources(this.opts.docker, ctx.nodeId, ctx.jobId).catch(
        () => ({ containers: [] as string[] }),
      );
      if (removed.containers.length > 0)
        ctx.system(`Removed ${removed.containers.length} leftover workflow container(s)`);
    }

    if (outcome.stopReason && stoppedAt !== undefined) parser.markCancelled(stoppedAt);
    // Steps without a reported result (e.g. no-op "Pre" steps) did not run.
    parser.finishRunning(outcome.stopReason ? 'cancelled' : 'skipped');
    const outputs: JobOutputs = { steps: parser.getSteps() };
    ctx.reportOutputs(outputs);
    if (outcome.spawnError) return failed(`Could not start act: ${outcome.spawnError}`);
    if (outcome.stopReason === 'cancelled') return { ...cancelled(), outputs };
    if (outcome.stopReason === 'timeout')
      return { ...failed(`Timed out after ${spec.timeoutSeconds}s`, outcome.exitCode), outputs };

    const artifacts = [];
    for (const file of await listFiles(artifactDir)) {
      // act stores uploads as <run id>/<artifact name>/<file>; drop the run id.
      const rel = relative(artifactDir, file).split(sep);
      const name = (rel.length > 1 ? rel.slice(1) : rel).join('/');
      try {
        const size = (await stat(file)).size;
        const artifact = await ctx.uploadArtifact(name, file);
        ctx.system(`Artifact ${name} uploaded (${size} bytes)`);
        artifacts.push(artifact);
      } catch (err) {
        ctx.system(`Artifact ${name}: upload failed: ${(err as Error).message}`);
      }
    }

    if (outcome.exitCode === 0)
      return {
        outcome: 'succeeded',
        exitCode: 0,
        error: null,
        oomKilled: false,
        outputs,
        artifacts,
      };
    const failedJobs = parser.failedJobs();
    return {
      outcome: 'failed',
      exitCode: outcome.exitCode,
      error:
        failedJobs.length > 0
          ? `Workflow job(s) failed: ${failedJobs.join(', ')}`
          : `act exited with code ${outcome.exitCode}`,
      oomKilled: false,
      outputs,
      artifacts,
    };
  }
}

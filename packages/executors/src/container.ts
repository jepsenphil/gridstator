import { randomBytes } from 'node:crypto';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { JobSpec } from '@gridstator/protocol';
import {
  type DockerCli,
  LABEL_ASSIGNMENT,
  LABEL_CACHE,
  LABEL_JOB,
  LABEL_MANAGED,
  LABEL_NODE,
} from './docker.js';
import {
  cancelled,
  type ExecutionContext,
  type ExecutionResult,
  type Executor,
} from './executor.js';

export const WORKSPACE_DIR = '/workspace';
export const CACHE_DIR = '/gridstator/cache';
export const CACHE_VOLUME = 'gridstator-cache';

/** Package-manager caches redirected into the shared cache volume (ADR-10). */
export const CACHE_ENV: Readonly<Record<string, string>> = {
  npm_config_cache: `${CACHE_DIR}/npm`,
  npm_config_store_dir: `${CACHE_DIR}/pnpm-store`,
  YARN_CACHE_FOLDER: `${CACHE_DIR}/yarn`,
  COREPACK_HOME: `${CACHE_DIR}/corepack`,
  COREPACK_ENABLE_DOWNLOAD_PROMPT: '0',
  PIP_CACHE_DIR: `${CACHE_DIR}/pip`,
  GOMODCACHE: `${CACHE_DIR}/go/mod`,
  GOCACHE: `${CACHE_DIR}/go/build`,
};

/** Names the docker CLI itself reads; these can never be passed via the CLI's environment. */
const RESERVED_ENV =
  /^(PATH|HOME|USER|SHELL|TMPDIR|TEMP|TMP|DOCKER_.*|BUILDKIT_.*|HTTPS?_PROXY|NO_PROXY|https?_proxy|no_proxy)$/;

export function containerName(jobId: string): string {
  return `gridstator-${jobId.toLowerCase()}`;
}

export function workspaceVolumeName(jobId: string): string {
  return `gridstator-ws-${jobId.toLowerCase()}`;
}

export interface ContainerExecutorOptions {
  docker: DockerCli;
  /** Private directory (mode 0700) for short-lived env files. */
  tmpDir: string;
  /** Seconds between SIGTERM and SIGKILL on cancel/timeout. */
  stopGraceSeconds?: number;
  pidsLimit?: number;
  /** Keep failed jobs' containers/volumes for inspection (the agent garbage-collects them later). */
  keepFailed?: boolean;
}

interface EnvPlan {
  envFile: string;
  envFileContent: string;
  /** Multi-line values passed via `-e NAME` with the value in the docker CLI's environment. */
  passThrough: Record<string, string>;
}

export function planEnvironment(
  spec: JobSpec,
  ctx: Pick<ExecutionContext, 'jobId' | 'attempt' | 'nodeId' | 'nodeName'>,
  envFile: string,
): EnvPlan {
  const merged: Record<string, string> = {
    CI: 'true',
    GRIDSTATOR: '1',
    GRIDSTATOR_JOB_ID: ctx.jobId,
    GRIDSTATOR_ATTEMPT: String(ctx.attempt),
    GRIDSTATOR_NODE_ID: ctx.nodeId,
    GRIDSTATOR_NODE_NAME: ctx.nodeName,
    ...CACHE_ENV,
    ...spec.env,
    ...spec.secretEnv,
  };
  const lines: string[] = [];
  const passThrough: Record<string, string> = {};
  for (const [name, value] of Object.entries(merged)) {
    if (/[\r\n]/.test(value)) {
      if (RESERVED_ENV.test(name)) {
        throw new Error(`Environment variable ${name} cannot contain line breaks`);
      }
      passThrough[name] = value;
    } else {
      lines.push(`${name}=${value}`);
    }
  }
  return { envFile, envFileContent: lines.join('\n') + '\n', passThrough };
}

/** Runs a command job in a fresh, labelled, resource-limited container (see ARCHITECTURE §3.2). */
export class ContainerExecutor implements Executor {
  readonly kind = 'command' as const;
  private readonly docker: DockerCli;
  private readonly tmpDir: string;
  private readonly grace: number;
  private readonly pidsLimit: number;
  private readonly keepFailed: boolean;

  constructor(options: ContainerExecutorOptions) {
    this.docker = options.docker;
    this.tmpDir = options.tmpDir;
    this.grace = options.stopGraceSeconds ?? 10;
    this.pidsLimit = options.pidsLimit ?? 4096;
    this.keepFailed = options.keepFailed ?? false;
  }

  async run(ctx: ExecutionContext): Promise<ExecutionResult> {
    const spec = ctx.spec;
    if (spec.kind !== 'command')
      throw new Error(`ContainerExecutor cannot run ${(spec as { kind: string }).kind} jobs`);
    const name = containerName(ctx.jobId);
    const volume = workspaceVolumeName(ctx.jobId);
    const labels = {
      [LABEL_MANAGED]: 'true',
      [LABEL_JOB]: ctx.jobId,
      [LABEL_NODE]: ctx.nodeId,
      [LABEL_ASSIGNMENT]: ctx.assignmentId,
    };
    let created = false;
    let result: ExecutionResult | undefined;
    try {
      if (ctx.signal.aborted) return cancelled();
      // A container with this name can only be left over from an earlier attempt on this node.
      await this.docker.removeContainer(name);
      await this.docker.ensureVolume(volume, labels);
      await this.docker.ensureVolume(CACHE_VOLUME, {
        [LABEL_MANAGED]: 'true',
        [LABEL_CACHE]: 'true',
      });

      const envFile = join(this.tmpDir, `${ctx.jobId}-${randomBytes(4).toString('hex')}.env`);
      const env = planEnvironment(spec, ctx, envFile);
      const args = [
        'create',
        '--name',
        name,
        '--init',
        '--pull',
        'missing',
        '--cpus',
        String(spec.resources.cpus),
        '--memory',
        `${spec.resources.memoryMb}m`,
        '--memory-swap',
        `${spec.resources.memoryMb}m`,
        '--pids-limit',
        String(this.pidsLimit),
        '--security-opt',
        'no-new-privileges',
        '--mount',
        `type=volume,src=${volume},dst=${WORKSPACE_DIR}`,
        '--mount',
        `type=volume,src=${CACHE_VOLUME},dst=${CACHE_DIR}`,
        '--workdir',
        spec.workdir === '.' ? WORKSPACE_DIR : `${WORKSPACE_DIR}/${spec.workdir}`,
        '--env-file',
        envFile,
      ];
      for (const [k, v] of Object.entries(labels)) args.push('--label', `${k}=${v}`);
      for (const k of Object.keys(env.passThrough)) args.push('--env', k);
      args.push(spec.image, ...spec.command);

      ctx.system(
        `Creating container on ${ctx.nodeName} (image ${spec.image}, ${spec.resources.cpus} CPU, ${spec.resources.memoryMb} MiB)`,
      );
      await writeFile(envFile, env.envFileContent, { mode: 0o600 });
      let createRes;
      try {
        createRes = await this.docker.exec(args, {
          env: { ...process.env, ...env.passThrough },
          onStderr: (chunk) => {
            const text = chunk.toString('utf8').trim();
            if (text) ctx.system(text);
          },
        });
      } finally {
        await rm(envFile, { force: true });
      }
      if (createRes.code !== 0) {
        return (result = {
          outcome: 'failed',
          exitCode: null,
          error: `Could not create container: ${createRes.stderr.trim().split('\n').slice(-1)[0] ?? 'unknown error'}`,
          oomKilled: false,
        });
      }
      created = true;
      if (ctx.signal.aborted) return (result = cancelled());

      if (ctx.workspaceTar) {
        ctx.system('Copying workspace snapshot into the container');
        await this.docker.copyInto(name, WORKSPACE_DIR, ctx.workspaceTar());
      }
      if (ctx.signal.aborted) return (result = cancelled());

      result = await this.attachAndWait(ctx, name, spec.timeoutSeconds);
      if (spec.artifacts.length > 0 && result.outcome !== 'cancelled') {
        result = { ...result, artifacts: await this.collectArtifacts(ctx, name, spec.artifacts) };
      }
      return result;
    } catch (err) {
      return (result = {
        outcome: 'failed',
        exitCode: null,
        error: (err as Error).message,
        oomKilled: false,
      });
    } finally {
      const keep = this.keepFailed && result?.outcome === 'failed';
      if (!keep) {
        if (created) await this.docker.removeContainer(name).catch(() => undefined);
        await this.docker.removeVolume(volume).catch(() => undefined);
      } else {
        ctx.system(`Keeping container ${name} and volume ${volume} for inspection`);
      }
    }
  }

  /** Copies each artifact path out of the stopped container as a tar and uploads it. */
  private async collectArtifacts(
    ctx: ExecutionContext,
    container: string,
    paths: readonly string[],
  ) {
    const collected = [];
    const dir = join(ctx.workDir, 'artifacts');
    await mkdir(dir, { recursive: true, mode: 0o700 });
    for (const [i, path] of paths.entries()) {
      const file = join(dir, `${i}.tar`);
      const res = await this.docker.copyOut(
        container,
        path === '.' ? WORKSPACE_DIR : `${WORKSPACE_DIR}/${path}`,
        file,
      );
      if (!res.ok) {
        ctx.system(`Artifact ${path}: not found in the workspace (${res.error})`);
        continue;
      }
      try {
        const artifact = await ctx.uploadArtifact(`${path === '.' ? 'workspace' : path}.tar`, file);
        ctx.system(`Artifact ${artifact.name} uploaded (${artifact.size} bytes)`);
        collected.push(artifact);
      } catch (err) {
        ctx.system(`Artifact ${path}: upload failed: ${(err as Error).message}`);
      }
    }
    return collected;
  }

  private attachAndWait(
    ctx: ExecutionContext,
    name: string,
    timeoutSeconds: number | undefined,
  ): Promise<ExecutionResult> {
    return new Promise<ExecutionResult>((resolve) => {
      const child = this.docker.spawn(['start', '--attach', name]);
      child.stdin?.end();
      child.stdout?.on('data', (c: Buffer) => ctx.output('stdout', c));
      child.stderr?.on('data', (c: Buffer) => ctx.output('stderr', c));
      let stopReason: 'cancelled' | 'timeout' | undefined;
      const stop = (reason: 'cancelled' | 'timeout') => {
        if (stopReason) return;
        stopReason = reason;
        ctx.system(
          reason === 'timeout'
            ? `Timed out after ${timeoutSeconds}s; stopping container`
            : 'Cancelling: stopping container',
        );
        void this.docker.stop(name, this.grace);
      };
      const onAbort = () => stop('cancelled');
      ctx.signal.addEventListener('abort', onAbort, { once: true });
      const timer = timeoutSeconds
        ? setTimeout(() => stop('timeout'), timeoutSeconds * 1000)
        : undefined;
      ctx.onRunning();
      child.on('error', (err) => {
        cleanup();
        resolve({
          outcome: 'failed',
          exitCode: null,
          error: `Could not start container: ${err.message}`,
          oomKilled: false,
        });
      });
      const cleanup = () => {
        ctx.signal.removeEventListener('abort', onAbort);
        if (timer) clearTimeout(timer);
      };
      child.on('close', () => {
        cleanup();
        void this.docker
          .inspectState(name)
          .then((state) => {
            if (stopReason === 'cancelled') {
              resolve({
                outcome: 'cancelled',
                exitCode: state.exitCode,
                error: null,
                oomKilled: false,
              });
            } else if (stopReason === 'timeout') {
              resolve({
                outcome: 'failed',
                exitCode: state.exitCode,
                error: `Timed out after ${timeoutSeconds}s`,
                oomKilled: false,
              });
            } else if (state.oomKilled) {
              resolve({
                outcome: 'failed',
                exitCode: state.exitCode,
                error: 'Container was killed because it ran out of memory',
                oomKilled: true,
              });
            } else if (state.exitCode === 0) {
              resolve({ outcome: 'succeeded', exitCode: 0, error: null, oomKilled: false });
            } else {
              resolve({
                outcome: 'failed',
                exitCode: state.exitCode,
                error: state.error ? state.error : `Process exited with code ${state.exitCode}`,
                oomKilled: false,
              });
            }
          })
          .catch((err: unknown) => {
            resolve({
              outcome: 'failed',
              exitCode: null,
              error: `Could not inspect container: ${(err as Error).message}`,
              oomKilled: false,
            });
          });
      });
    });
  }
}

/**
 * Removes this node's Gridstator-managed job containers and workspace volumes, except those for
 * `keepJobIds`. Never touches anything without both the managed label and this node's id, and never
 * removes the shared cache volume.
 */
export async function cleanupManagedResources(
  docker: DockerCli,
  nodeId: string,
  keepJobIds: ReadonlySet<string>,
): Promise<{ containers: string[]; volumes: string[] }> {
  const filters = [`${LABEL_MANAGED}=true`, `${LABEL_NODE}=${nodeId}`];
  const removedContainers: string[] = [];
  const removedVolumes: string[] = [];
  for (const c of await docker.listContainers(filters)) {
    const job = c.labels[LABEL_JOB];
    if (!job || keepJobIds.has(job)) continue;
    await docker.removeContainer(c.id);
    removedContainers.push(c.name);
  }
  for (const v of await docker.listVolumes(filters)) {
    const job = v.labels[LABEL_JOB];
    if (!job || keepJobIds.has(job) || v.labels[LABEL_CACHE]) continue;
    await docker.removeVolume(v.name);
    removedVolumes.push(v.name);
  }
  return { containers: removedContainers, volumes: removedVolumes };
}

/** Removes containers and volumes labelled for one job on this node (crash/cancel safety net). */
export async function cleanupJobResources(
  docker: DockerCli,
  nodeId: string,
  jobId: string,
): Promise<{ containers: string[]; volumes: string[] }> {
  const filters = [`${LABEL_MANAGED}=true`, `${LABEL_NODE}=${nodeId}`, `${LABEL_JOB}=${jobId}`];
  const containers: string[] = [];
  const volumes: string[] = [];
  for (const c of await docker.listContainers(filters)) {
    await docker.removeContainer(c.id);
    containers.push(c.name);
  }
  for (const v of await docker.listVolumes(filters)) {
    if (v.labels[LABEL_CACHE]) continue;
    await docker.removeVolume(v.name);
    volumes.push(v.name);
  }
  return { containers, volumes };
}

import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import type { DockerBuildJobSpec, ImageOutput } from '@gridstator/protocol';
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

/** Names the docker CLI itself reads; build secrets with these names cannot be passed via env. */
const RESERVED_ENV =
  /^(PATH|HOME|USER|SHELL|TMPDIR|TEMP|TMP|DOCKER_.*|BUILDKIT_.*|BUILDX_.*|HTTPS?_PROXY|NO_PROXY|https?_proxy|no_proxy)$/;

export interface DockerBuildExecutorOptions {
  docker: DockerCli;
  stopGraceSeconds?: number;
}

/** Temporary tag used to `docker save` an untagged image for tar output. */
export function exportTag(jobId: string): string {
  return `gridstator-export/${jobId.toLowerCase()}:latest`;
}

/** Builds the arguments for `docker buildx build` (exported for tests). */
export function buildxArgs(
  spec: DockerBuildJobSpec,
  paths: { dockerfile: string; context: string; metadataFile: string; tarFile: string },
  ids: { jobId: string; nodeId: string },
): string[] {
  const args = [
    'buildx',
    'build',
    '--progress=plain',
    '--file',
    paths.dockerfile,
    '--metadata-file',
    paths.metadataFile,
  ];
  args.push(
    '--label',
    `${LABEL_MANAGED}=true`,
    '--label',
    `${LABEL_JOB}=${ids.jobId}`,
    '--label',
    `${LABEL_NODE}=${ids.nodeId}`,
  );
  if (spec.stage) args.push('--target', spec.stage);
  for (const [k, v] of Object.entries(spec.buildArgs)) args.push('--build-arg', `${k}=${v}`);
  for (const name of Object.keys(spec.secretEnv)) args.push('--secret', `id=${name},env=${name}`);
  for (const tag of spec.tags) args.push('--tag', tag);
  if (spec.platforms.length > 0) args.push('--platform', spec.platforms.join(','));
  if (spec.noCache) args.push('--no-cache');
  if (spec.pull) args.push('--pull');
  for (const c of spec.cacheFrom) args.push('--cache-from', c);
  for (const c of spec.cacheTo) args.push('--cache-to', c);
  switch (spec.output) {
    case 'load':
      args.push('--load');
      break;
    case 'push':
      args.push('--push');
      break;
    case 'tar':
      // Exporting straight to a file is unsupported by Docker's default "docker" build driver;
      // load the image and `docker save` it afterwards, which works with every driver.
      args.push('--load');
      if (spec.tags.length === 0) args.push('--tag', exportTag(ids.jobId));
      break;
    case 'none':
      args.push('--output', 'type=cacheonly');
      break;
  }
  args.push(paths.context);
  return args;
}

/**
 * Builds a Docker image from the job's snapshot with BuildKit (`docker buildx build`). The build
 * cache lives in the worker's BuildKit instance, so repeat builds on the same worker are fast; the
 * scheduler's project affinity prefers that worker.
 */
export class DockerBuildExecutor implements Executor {
  readonly kind = 'docker-build' as const;
  private readonly docker: DockerCli;
  private readonly grace: number;

  constructor(options: DockerBuildExecutorOptions) {
    this.docker = options.docker;
    this.grace = options.stopGraceSeconds ?? 10;
  }

  async run(ctx: ExecutionContext): Promise<ExecutionResult> {
    const spec = ctx.spec;
    if (spec.kind !== 'docker-build')
      return failed(`DockerBuildExecutor cannot run ${spec.kind} jobs`);
    if (!ctx.materialize) return failed('docker-build jobs need a source snapshot');
    if (spec.output === 'push' && spec.tags.length === 0)
      return failed('output "push" needs at least one --tag');
    for (const name of Object.keys(spec.secretEnv)) {
      if (RESERVED_ENV.test(name))
        return failed(`Build secret name ${name} is reserved; choose another name`);
    }
    if (ctx.signal.aborted) return cancelled();

    const src = join(ctx.workDir, 'src');
    ctx.system('Staging build context from the snapshot');
    await ctx.materialize(src);
    const context = await resolveInside(src, spec.context, 'directory');
    if ('error' in context) return failed(`Build context ${context.error}`);
    const dockerfileRel =
      spec.dockerfile ?? (spec.context === '.' ? 'Dockerfile' : `${spec.context}/Dockerfile`);
    const dockerfile = await resolveInside(src, dockerfileRel, 'file');
    if ('error' in dockerfile) return failed(`Dockerfile ${dockerfile.error}`);

    const metadataFile = join(ctx.workDir, 'metadata.json');
    const tarFile = join(ctx.workDir, 'image.tar');
    const args = buildxArgs(
      spec,
      { dockerfile: dockerfile.path, context: context.path, metadataFile, tarFile },
      { jobId: ctx.jobId, nodeId: ctx.nodeId },
    );
    ctx.system(
      `docker buildx build ${dockerfileRel}${spec.stage ? ` --target ${spec.stage}` : ''} (output: ${spec.output}) on ${ctx.nodeName}`,
    );
    const outcome = await runProcess(ctx, {
      bin: this.docker.bin,
      args,
      env: { ...process.env, ...spec.secretEnv, BUILDX_NO_DEFAULT_ATTESTATIONS: '1' },
      timeoutSeconds: spec.timeoutSeconds,
      graceSeconds: this.grace,
    });
    if (outcome.spawnError) return failed(`Could not start docker buildx: ${outcome.spawnError}`);
    if (outcome.stopReason === 'cancelled') return cancelled();
    if (outcome.stopReason === 'timeout')
      return failed(`Timed out after ${spec.timeoutSeconds}s`, outcome.exitCode);
    if (outcome.exitCode !== 0)
      return failed(`docker buildx build exited with code ${outcome.exitCode}`, outcome.exitCode);

    const image = await this.describeImage(spec, metadataFile);
    const result: ExecutionResult = {
      outcome: 'succeeded',
      exitCode: 0,
      error: null,
      oomKilled: false,
      outputs: { image },
    };
    if (image.id)
      ctx.system(
        `Image ${image.id.slice(0, 19)}${image.tags.length ? ` tagged ${image.tags.join(', ')}` : ''}${image.sizeBytes ? ` (${Math.round(image.sizeBytes / 1024 / 1024)} MiB)` : ''}`,
      );
    if (spec.output === 'tar') {
      const saveRefs = spec.tags.length > 0 ? spec.tags : [exportTag(ctx.jobId)];
      ctx.system(`Exporting the image with docker save (${saveRefs.join(', ')})`);
      const saved = await this.docker.exec(['save', '--output', tarFile, ...saveRefs]);
      if (spec.tags.length === 0) await this.docker.exec(['image', 'rm', exportTag(ctx.jobId)]);
      if (saved.code !== 0) return failed(`docker save failed: ${saved.stderr.trim()}`);
      try {
        await stat(tarFile);
        const artifact = await ctx.uploadArtifact('image.tar', tarFile);
        ctx.system(
          `Exported image uploaded as artifact image.tar (${Math.round(artifact.size / 1024 / 1024)} MiB)`,
        );
        result.artifacts = [artifact];
      } catch (err) {
        return failed(`Could not upload the exported image: ${(err as Error).message}`);
      }
    }
    return result;
  }

  private async describeImage(
    spec: DockerBuildJobSpec,
    metadataFile: string,
  ): Promise<ImageOutput> {
    let metadata: Record<string, unknown> = {};
    try {
      metadata = JSON.parse(await readFile(metadataFile, 'utf8')) as Record<string, unknown>;
    } catch {
      // buildx writes no metadata for some outputs (e.g. cacheonly)
    }
    const str = (k: string) => (typeof metadata[k] === 'string' ? metadata[k] : null);
    const id = str('containerimage.config.digest');
    let sizeBytes: number | null = null;
    if (id && (spec.output === 'load' || spec.output === 'tar')) {
      const res = await this.docker.exec(['image', 'inspect', '--format', '{{.Size}}', id]);
      if (res.code === 0 && /^\d+$/.test(res.stdout.trim())) sizeBytes = Number(res.stdout.trim());
    }
    return {
      id,
      digest: str('containerimage.digest'),
      tags: spec.tags,
      sizeBytes,
      output: spec.output,
    };
  }
}

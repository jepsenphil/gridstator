import { z } from 'zod';
import {
  EnvMapSchema,
  IdSchema,
  LabelSchema,
  SnapshotIdSchema,
  TimestampSchema,
} from './common.js';
import { JobStateSchema } from './states.js';

export const DEFAULT_IMAGE = 'node:24-bookworm';
export const DEFAULT_RESOURCES = { cpus: 2, memoryMb: 4096 } as const;

export const ResourceRequestSchema = z.object({
  cpus: z.number().positive().max(1024),
  memoryMb: z
    .number()
    .int()
    .min(16)
    .max(16 * 1024 * 1024),
});
export type ResourceRequest = z.infer<typeof ResourceRequestSchema>;

export const JobTargetSchema = z.object({
  /** Node id or node name. When set, only that node is eligible. */
  node: z.string().min(1).max(128).optional(),
  /** Every label must be present on the node. */
  labels: z.array(LabelSchema).max(32).default([]),
  /** Preferred node ids or names: chosen when eligible, but the job may run elsewhere. */
  prefer: z.array(z.string().min(1).max(128)).max(16).default([]),
});
export type JobTarget = z.infer<typeof JobTargetSchema>;

export const RetryOnSchema = z.enum(['lost', 'failed']);
export const RetryPolicySchema = z.object({
  max: z.number().int().min(0).max(10).default(0),
  on: z.array(RetryOnSchema).default(['lost']),
});
export type RetryPolicy = z.infer<typeof RetryPolicySchema>;

/** Workdir inside the snapshot, relative and normalized ("." for the root). */
export const WorkdirSchema = z
  .string()
  .max(1024)
  .refine(
    (p) =>
      p === '.' ||
      (!p.startsWith('/') &&
        !p.includes('\\') &&
        p.split('/').every((s) => s !== '' && s !== '.' && s !== '..')),
    'workdir must be "." or a normalized relative path without ".."',
  );

const BaseJobSpecShape = {
  name: z.string().min(1).max(200).optional(),
  env: EnvMapSchema.default({}),
  /** Values are masked in logs and redacted in every API response. */
  secretEnv: EnvMapSchema.default({}),
  snapshot: SnapshotIdSchema.optional(),
  workdir: WorkdirSchema.default('.'),
  resources: ResourceRequestSchema.default({ ...DEFAULT_RESOURCES }),
  target: JobTargetSchema.default({ labels: [], prefer: [] }),
  timeoutSeconds: z
    .number()
    .int()
    .positive()
    .max(7 * 24 * 3600)
    .optional(),
  retry: RetryPolicySchema.default({ max: 0, on: ['lost'] }),
  priority: z.number().int().min(-100).max(100).default(0),
  /** Cache-affinity key; jobs from the same project prefer nodes with warm caches. */
  project: z.string().min(1).max(200).optional(),
};

/** A normalized path relative to the snapshot root ("." for the root). */
export const RelPathSchema = z
  .string()
  .min(1)
  .max(1024)
  .refine(
    (p) =>
      p === '.' ||
      (!p.startsWith('/') &&
        !p.includes('\\') &&
        !/^[A-Za-z]:/.test(p) &&
        p.split('/').every((s) => s !== '' && s !== '.' && s !== '..')),
    'expected "." or a normalized relative path without ".."',
  );

export const CommandJobSpecSchema = z.object({
  kind: z.literal('command'),
  image: z.string().min(1).max(512).default(DEFAULT_IMAGE),
  /** argv executed in the container. Shell strings are sent as ["sh", "-c", "<string>"]. */
  command: z
    .array(z.string().max(256 * 1024))
    .min(1)
    .max(4096),
  /** Files or directories (relative to the snapshot root) collected as artifacts after the run. */
  artifacts: z.array(RelPathSchema).max(32).default([]),
  ...BaseJobSpecShape,
});

export const BuildOutputSchema = z.enum(['load', 'push', 'tar', 'none']);
export type BuildOutput = z.infer<typeof BuildOutputSchema>;

export const DockerBuildJobSpecSchema = z.object({
  kind: z.literal('docker-build'),
  /** Build context directory, relative to the snapshot root. */
  context: RelPathSchema.default('.'),
  /** Dockerfile path relative to the snapshot root. Default: `<context>/Dockerfile`. */
  dockerfile: RelPathSchema.optional(),
  /** Dockerfile stage to build (`--target`). */
  stage: z.string().min(1).max(128).optional(),
  buildArgs: z
    .record(z.string().regex(/^[A-Za-z_][A-Za-z0-9_.-]*$/), z.string().max(64 * 1024))
    .default({}),
  tags: z.array(z.string().min(1).max(512)).max(32).default([]),
  platforms: z.array(z.string().min(1).max(64)).max(8).default([]),
  noCache: z.boolean().default(false),
  /** Always pull newer base images. */
  pull: z.boolean().default(false),
  /**
   * load: into the worker's image store (default); push: to the registry named by the tags;
   * tar: export as an artifact (image.tar); none: build only (validates the Dockerfile, warms cache).
   */
  output: BuildOutputSchema.default('load'),
  cacheFrom: z.array(z.string().min(1).max(512)).max(8).default([]),
  cacheTo: z.array(z.string().min(1).max(512)).max(8).default([]),
  ...BaseJobSpecShape,
});

export const ActionsJobSpecSchema = z.object({
  kind: z.literal('actions'),
  /** Workflow file relative to the snapshot root, e.g. ".github/workflows/ci.yml". */
  workflow: RelPathSchema,
  /** Run only this job id (act -j). Default: the whole workflow for the event. */
  job: z.string().min(1).max(200).optional(),
  event: z
    .string()
    .regex(/^[a-z_]+$/)
    .default('push'),
  /** Event payload (github.event); act generates a minimal one when omitted. */
  eventPayload: z.record(z.string(), z.unknown()).optional(),
  /** workflow_dispatch / workflow_call inputs. */
  inputs: z.record(z.string().min(1).max(200), z.string().max(64 * 1024)).default({}),
  /** Values for the `vars` context. */
  vars: EnvMapSchema.default({}),
  /** runs-on label → container image overrides. */
  platforms: z.record(z.string().min(1).max(100), z.string().min(1).max(512)).default({}),
  /** Restrict matrix combinations, e.g. ["node:20"]. */
  matrix: z
    .array(
      z
        .string()
        .max(200)
        .regex(/^[^:]+:.+$/),
    )
    .max(32)
    .default([]),
  /** Mount the worker's Docker socket into job containers (needed by workflows that run docker). */
  dockerSocket: z.boolean().default(false),
  /** Git context captured on the client, so github.ref / github.repository match the developer's checkout. */
  git: z
    .object({
      branch: z.string().max(255).optional(),
      remoteUrl: z.string().max(1024).optional(),
      headSha: z.string().max(64).optional(),
    })
    .optional(),
  ...BaseJobSpecShape,
});

export const JobSpecSchema = z.discriminatedUnion('kind', [
  CommandJobSpecSchema,
  DockerBuildJobSpecSchema,
  ActionsJobSpecSchema,
]);
export type JobSpec = z.infer<typeof JobSpecSchema>;
export type JobSpecInput = z.input<typeof JobSpecSchema>;
export type JobKind = JobSpec['kind'];
export type CommandJobSpec = z.infer<typeof CommandJobSpecSchema>;
export type DockerBuildJobSpec = z.infer<typeof DockerBuildJobSpecSchema>;
export type ActionsJobSpec = z.infer<typeof ActionsJobSpecSchema>;
export const JOB_KINDS: readonly JobKind[] = ['command', 'docker-build', 'actions'];

export const JobStepStatusSchema = z.enum([
  'running',
  'success',
  'failure',
  'skipped',
  'cancelled',
]);
export type JobStepStatus = z.infer<typeof JobStepStatusSchema>;

/** One execution step (e.g. a GitHub Actions step), for progress display. */
export const JobStepSchema = z.object({
  id: z.string().max(300),
  /** Grouping, e.g. the workflow job ("CI/test"). */
  group: z.string().max(300).nullable(),
  name: z.string().max(1000),
  status: JobStepStatusSchema,
  startedAt: TimestampSchema.nullable(),
  durationMs: z.number().nonnegative().nullable(),
});
export type JobStep = z.infer<typeof JobStepSchema>;

export const ImageOutputSchema = z.object({
  /** Image config digest (the local image ID). */
  id: z.string().nullable(),
  /** Manifest digest (registry), when known. */
  digest: z.string().nullable(),
  tags: z.array(z.string()),
  sizeBytes: z.number().int().nonnegative().nullable(),
  output: BuildOutputSchema,
});
export type ImageOutput = z.infer<typeof ImageOutputSchema>;

/** Structured results beyond the exit code; filled in by executors as the job runs. */
export const JobOutputsSchema = z.object({
  image: ImageOutputSchema.optional(),
  steps: z.array(JobStepSchema).max(5000).optional(),
});
export type JobOutputs = z.infer<typeof JobOutputsSchema>;

export const ArtifactSchema = z.object({
  /** Relative, path-like name, e.g. "image.tar" or "report/report.zip". */
  name: z.string().min(1).max(1024),
  digest: SnapshotIdSchema,
  size: z.number().int().nonnegative(),
});
export type Artifact = z.infer<typeof ArtifactSchema>;

export const JobSchema = z.object({
  id: IdSchema,
  name: z.string(),
  kind: z.string(),
  state: JobStateSchema,
  stateReason: z.string().nullable(),
  priority: z.number().int(),
  owner: z.string(),
  /** Spec as submitted, with secretEnv values replaced by "***". */
  spec: JobSpecSchema,
  snapshotId: SnapshotIdSchema.nullable(),
  nodeId: z.string().nullable(),
  nodeName: z.string().nullable(),
  assignmentId: z.string().nullable(),
  attempt: z.number().int().positive(),
  retryOf: z.string().nullable(),
  exitCode: z.number().int().nullable(),
  error: z.string().nullable(),
  queuePosition: z.number().int().positive().nullable(),
  logSeq: z.number().int().nonnegative(),
  outputs: JobOutputsSchema.nullable(),
  artifacts: z.array(ArtifactSchema),
  /** Set for jobs that are shards of a job group. */
  groupId: z.string().nullable(),
  shardIndex: z.number().int().positive().nullable(),
  shardTotal: z.number().int().positive().nullable(),
  createdAt: TimestampSchema,
  queuedAt: TimestampSchema.nullable(),
  assignedAt: TimestampSchema.nullable(),
  startedAt: TimestampSchema.nullable(),
  finishedAt: TimestampSchema.nullable(),
  cancelRequestedAt: TimestampSchema.nullable(),
});
export type Job = z.infer<typeof JobSchema>;

export const LogStreamSchema = z.enum(['stdout', 'stderr', 'system']);
export type LogStream = z.infer<typeof LogStreamSchema>;

/** One log chunk. Compact keys because logs dominate traffic and storage. */
export const LogEntrySchema = z.object({
  /** Per-job sequence number, starting at 1. */
  s: z.number().int().positive(),
  /** Epoch ms. */
  t: TimestampSchema,
  o: LogStreamSchema,
  d: z.string(),
});
export type LogEntry = z.infer<typeof LogEntrySchema>;

export const JobEventSchema = z.object({
  id: z.number().int(),
  jobId: z.string(),
  ts: TimestampSchema,
  type: z.string(),
  fromState: JobStateSchema.nullable(),
  toState: JobStateSchema.nullable(),
  actor: z.string(),
  message: z.string().nullable(),
});
export type JobEvent = z.infer<typeof JobEventSchema>;

export const SECRET_PLACEHOLDER = '***';

/** Returns a copy of the spec with secret values replaced, safe for API responses and logs. */
export function redactSpec(spec: JobSpec): JobSpec {
  const secretEnv: Record<string, string> = {};
  for (const key of Object.keys(spec.secretEnv)) secretEnv[key] = SECRET_PLACEHOLDER;
  return { ...spec, secretEnv };
}

export function jobDisplayName(spec: JobSpec): string {
  if (spec.name) return spec.name;
  const clip = (t: string) => (t.length > 80 ? `${t.slice(0, 77)}...` : t);
  switch (spec.kind) {
    case 'command': {
      const cmd = spec.command;
      const shown = cmd.length >= 3 && cmd[0] === 'sh' && cmd[1] === '-c' ? cmd[2] : cmd.join(' ');
      return clip(shown ?? '');
    }
    case 'docker-build': {
      const file =
        spec.dockerfile ?? (spec.context === '.' ? 'Dockerfile' : `${spec.context}/Dockerfile`);
      return clip(`build ${file}${spec.stage ? ` --target ${spec.stage}` : ''}`);
    }
    case 'actions': {
      const base = spec.workflow.split('/').pop() ?? spec.workflow;
      return clip(`actions ${base}${spec.job ? ` (${spec.job})` : ''} on ${spec.event}`);
    }
  }
}

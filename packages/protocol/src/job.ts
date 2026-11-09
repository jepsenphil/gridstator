import { z } from 'zod';
import {
  EnvMapSchema,
  IdSchema,
  LabelSchema,
  SnapshotIdSchema,
  TimestampSchema,
} from './common.js';
import { JobStateSchema } from './states.js';

export const DEFAULT_IMAGE = 'node:24-bookworm-slim';
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
  target: JobTargetSchema.default({ labels: [] }),
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

export const CommandJobSpecSchema = z.object({
  kind: z.literal('command'),
  image: z.string().min(1).max(512).default(DEFAULT_IMAGE),
  /** argv executed in the container. Shell strings are sent as ["sh", "-c", "<string>"]. */
  command: z
    .array(z.string().max(256 * 1024))
    .min(1)
    .max(4096),
  ...BaseJobSpecShape,
});

export const JobSpecSchema = z.discriminatedUnion('kind', [CommandJobSpecSchema]);
export type JobSpec = z.infer<typeof JobSpecSchema>;
export type JobSpecInput = z.input<typeof JobSpecSchema>;
export type JobKind = JobSpec['kind'];

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
  const cmd = spec.command;
  const shown = cmd.length >= 3 && cmd[0] === 'sh' && cmd[1] === '-c' ? cmd[2] : cmd.join(' ');
  return (shown ?? '').length > 80 ? `${(shown ?? '').slice(0, 77)}...` : (shown ?? '');
}

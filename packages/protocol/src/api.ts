import { z } from 'zod';
import { DigestSchema, TimestampSchema } from './common.js';
import { JobSpecSchema, JobSchema, LogEntrySchema } from './job.js';
import { NodeInfoSchema, NodeSettingsPatchSchema, NodeSchema } from './node.js';
import { JobStateSchema } from './states.js';

export const API_PREFIX = '/v1';

export const SubmitJobRequestSchema = z.object({ spec: JobSpecSchema });
export type SubmitJobRequest = z.input<typeof SubmitJobRequestSchema>;

export const ListJobsQuerySchema = z.object({
  state: z
    .string()
    .optional()
    .transform((s) => (s ? s.split(',').map((x) => JobStateSchema.parse(x.trim())) : undefined)),
  node: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(500).default(50),
  /** Return jobs with an id lexicographically lower than this (older), for pagination. */
  before: z.string().optional(),
});

export const ListJobsResponseSchema = z.object({ jobs: z.array(JobSchema) });
export type ListJobsResponse = z.infer<typeof ListJobsResponseSchema>;

export const LogsQuerySchema = z.object({
  after: z.coerce.number().int().min(0).default(0),
  limit: z.coerce.number().int().min(1).max(100_000).default(10_000),
});
export const LogsResponseSchema = z.object({
  entries: z.array(LogEntrySchema),
  /** True when the job is terminal and no entries exist after the last one returned. */
  complete: z.boolean(),
});
export type LogsResponse = z.infer<typeof LogsResponseSchema>;

export const RegisterAgentRequestSchema = z.object({
  name: z
    .string()
    .min(1)
    .max(64)
    .regex(/^[A-Za-z0-9][A-Za-z0-9._-]*$/, 'node names may contain letters, digits, ".", "_", "-"'),
  info: NodeInfoSchema,
  settings: NodeSettingsPatchSchema.optional(),
});
export type RegisterAgentRequest = z.infer<typeof RegisterAgentRequestSchema>;

export const RegisterAgentResponseSchema = z.object({
  nodeId: z.string(),
  name: z.string(),
  credential: z.string(),
});
export type RegisterAgentResponse = z.infer<typeof RegisterAgentResponseSchema>;

export const PatchNodeRequestSchema = NodeSettingsPatchSchema;
export const ListNodesResponseSchema = z.object({ nodes: z.array(NodeSchema) });
export type ListNodesResponse = z.infer<typeof ListNodesResponseSchema>;

export const TokenRoleSchema = z.enum(['admin', 'operator']);
export type TokenRole = z.infer<typeof TokenRoleSchema>;

export const CreateTokenRequestSchema = z.object({
  kind: z.enum(['client', 'join']),
  name: z.string().min(1).max(64),
  role: TokenRoleSchema.default('operator'),
  /** Defaults: never for client tokens, 24h for join tokens. */
  ttlSeconds: z.number().int().positive().optional(),
  /** Join tokens only. Default 1. */
  maxUses: z.number().int().positive().max(1000).optional(),
});
export type CreateTokenRequest = z.input<typeof CreateTokenRequestSchema>;

export const TokenViewSchema = z.object({
  id: z.string(),
  kind: z.enum(['client', 'join', 'agent']),
  name: z.string(),
  role: TokenRoleSchema.nullable(),
  nodeId: z.string().nullable(),
  createdAt: TimestampSchema,
  expiresAt: TimestampSchema.nullable(),
  usesRemaining: z.number().int().nullable(),
  lastUsedAt: TimestampSchema.nullable(),
  revokedAt: TimestampSchema.nullable(),
});
export type TokenView = z.infer<typeof TokenViewSchema>;

export const CreateTokenResponseSchema = z.object({ token: z.string(), info: TokenViewSchema });
export type CreateTokenResponse = z.infer<typeof CreateTokenResponseSchema>;

export const BlobDigestsRequestSchema = z.object({
  digests: z.array(DigestSchema).max(1_000_000),
});
export const BlobsMissingResponseSchema = z.object({ missing: z.array(DigestSchema) });
export const BlobsUploadResponseSchema = z.object({
  stored: z.number().int(),
  alreadyPresent: z.number().int(),
  bytes: z.number().int(),
});
export type BlobsUploadResponse = z.infer<typeof BlobsUploadResponseSchema>;

export const CreateSnapshotRequestSchema = z.object({ manifestDigest: DigestSchema });
export const SnapshotViewSchema = z.object({
  id: DigestSchema,
  fileCount: z.number().int(),
  totalBytes: z.number().int(),
  createdAt: TimestampSchema,
  lastUsedAt: TimestampSchema,
  owner: z.string(),
});
export type SnapshotView = z.infer<typeof SnapshotViewSchema>;

export const ClusterViewSchema = z.object({
  clusterId: z.string(),
  version: z.string(),
  caFingerprint: z.string(),
  startedAt: TimestampSchema,
  nodes: z.object({ total: z.number().int(), online: z.number().int() }),
  jobs: z.partialRecord(JobStateSchema, z.number().int()),
  whoami: z.object({ name: z.string(), role: TokenRoleSchema }),
});
export type ClusterView = z.infer<typeof ClusterViewSchema>;

export const HealthResponseSchema = z.object({
  ok: z.literal(true),
  service: z.literal('gridstator-controller'),
  version: z.string(),
  apiVersion: z.literal('v1'),
});
export type HealthResponse = z.infer<typeof HealthResponseSchema>;

export const ClusterSettingsSchema = z.object({
  /** Terminal jobs (and their logs) older than this are deleted. */
  jobRetentionDays: z.number().int().min(1).max(3650),
  /** Logs of terminal jobs older than this are deleted (the job record is kept). */
  logRetentionDays: z.number().int().min(1).max(3650),
  /** Snapshots unused for this long, and not referenced by active jobs, are deleted. */
  snapshotRetentionDays: z.number().int().min(1).max(3650),
});
export type ClusterSettings = z.infer<typeof ClusterSettingsSchema>;
export const ClusterSettingsPatchSchema = ClusterSettingsSchema.partial();
export const DEFAULT_CLUSTER_SETTINGS: ClusterSettings = {
  jobRetentionDays: 90,
  logRetentionDays: 30,
  snapshotRetentionDays: 7,
};

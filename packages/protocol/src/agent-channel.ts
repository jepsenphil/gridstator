import { z } from 'zod';
import { SnapshotIdSchema, TimestampSchema } from './common.js';
import { JobSpecSchema, LogEntrySchema } from './job.js';
import {
  NodeInfoSchema,
  NodeMetricsSchema,
  NodeSettingsPatchSchema,
  NodeSettingsSchema,
} from './node.js';

/** Bumped on incompatible agent-channel changes. Controller and agent must match. */
export const AGENT_PROTOCOL_VERSION = 1;

/** `lost` means the agent abandoned the job (e.g. it shut down); the user's code did not fail. */
export const AgentJobOutcomeSchema = z.enum(['succeeded', 'failed', 'cancelled', 'lost']);
export type AgentJobOutcome = z.infer<typeof AgentJobOutcomeSchema>;

export const AgentJobResultSchema = z.object({
  jobId: z.string(),
  assignmentId: z.string(),
  outcome: AgentJobOutcomeSchema,
  exitCode: z.number().int().nullable(),
  error: z.string().max(8192).nullable(),
  oomKilled: z.boolean().default(false),
  finishedAt: TimestampSchema,
  /** Last log sequence number written for this job. */
  logSeq: z.number().int().nonnegative(),
});
export type AgentJobResult = z.infer<typeof AgentJobResultSchema>;

export const AgentActiveJobSchema = z.object({
  jobId: z.string(),
  assignmentId: z.string(),
  phase: z.enum(['starting', 'running']),
  logSeq: z.number().int().nonnegative(),
});
export type AgentActiveJob = z.infer<typeof AgentActiveJobSchema>;

// ---- agent → controller -------------------------------------------------------------------

export const AgentHelloSchema = z.object({
  type: z.literal('hello'),
  protocolVersion: z.number().int(),
  info: NodeInfoSchema,
  /** Only settings the operator explicitly set on the agent command line. */
  settings: NodeSettingsPatchSchema.optional(),
  activeJobs: z.array(AgentActiveJobSchema),
  /** Terminal results the controller has not acknowledged yet. */
  results: z.array(AgentJobResultSchema),
});

export const AgentHeartbeatSchema = z.object({
  type: z.literal('heartbeat'),
  metrics: NodeMetricsSchema,
  activeJobs: z.array(z.string()),
});

export const AgentJobStateSchema = z.object({
  type: z.literal('job.state'),
  jobId: z.string(),
  assignmentId: z.string(),
  phase: z.enum(['starting', 'running']),
  ts: TimestampSchema,
});

export const AgentJobResultMessageSchema = z.object({
  type: z.literal('job.result'),
  result: AgentJobResultSchema,
});

export const AgentJobLogSchema = z.object({
  type: z.literal('job.log'),
  jobId: z.string(),
  assignmentId: z.string(),
  entries: z.array(LogEntrySchema).min(1).max(10_000),
});

export const AgentJobRejectedSchema = z.object({
  type: z.literal('job.rejected'),
  jobId: z.string(),
  assignmentId: z.string(),
  reason: z.string().max(4096),
});

export const AgentMessageSchema = z.discriminatedUnion('type', [
  AgentHelloSchema,
  AgentHeartbeatSchema,
  AgentJobStateSchema,
  AgentJobResultMessageSchema,
  AgentJobLogSchema,
  AgentJobRejectedSchema,
]);
export type AgentMessage = z.infer<typeof AgentMessageSchema>;
export type AgentHello = z.infer<typeof AgentHelloSchema>;

// ---- controller → agent -------------------------------------------------------------------

export const ControllerWelcomeSchema = z.object({
  type: z.literal('welcome'),
  nodeId: z.string(),
  name: z.string(),
  settings: NodeSettingsSchema,
  /** For active jobs the controller still tracks: replay logs after this sequence number. */
  resume: z.array(z.object({ jobId: z.string(), logSeq: z.number().int().nonnegative() })),
});

export const ControllerJobAssignSchema = z.object({
  type: z.literal('job.assign'),
  jobId: z.string(),
  assignmentId: z.string(),
  attempt: z.number().int().positive(),
  /** Full spec including secret values (the channel is authenticated and encrypted). */
  spec: JobSpecSchema,
  snapshotId: SnapshotIdSchema.nullable(),
});
export type ControllerJobAssign = z.infer<typeof ControllerJobAssignSchema>;

export const ControllerJobCancelSchema = z.object({
  type: z.literal('job.cancel'),
  jobId: z.string(),
  reason: z.string(),
});

export const ControllerJobAckSchema = z.object({
  type: z.literal('job.ack'),
  jobId: z.string(),
});

export const ControllerNodeSettingsSchema = z.object({
  type: z.literal('node.settings'),
  settings: NodeSettingsSchema,
});

export const ControllerErrorSchema = z.object({
  type: z.literal('error'),
  code: z.string(),
  message: z.string(),
  /** When true the agent should not reconnect automatically (e.g. credential revoked). */
  fatal: z.boolean(),
});

export const ControllerMessageSchema = z.discriminatedUnion('type', [
  ControllerWelcomeSchema,
  ControllerJobAssignSchema,
  ControllerJobCancelSchema,
  ControllerJobAckSchema,
  ControllerNodeSettingsSchema,
  ControllerErrorSchema,
]);
export type ControllerMessage = z.infer<typeof ControllerMessageSchema>;

import { z } from 'zod';
import { LabelSchema, TimestampSchema } from './common.js';
import { AgentBuildSchema, NodeAutoUpdateSchema, NodeUpdateStatusSchema } from './update.js';

export const NodeNameSchema = z
  .string()
  .min(1)
  .max(64)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._-]*$/, 'node names may contain letters, digits, ".", "_", "-"');

export const ToolStatusSchema = z.object({
  available: z.boolean(),
  version: z.string().optional(),
  error: z.string().optional(),
});
export type ToolStatus = z.infer<typeof ToolStatusSchema>;

/** Static facts about a worker, refreshed on every (re)connect. */
export const NodeInfoSchema = z.object({
  hostname: z.string().max(255),
  /** Opaque identity; distinguishes a controller's worker from hosts with the same name. */
  machineId: z.string().max(128).optional(),
  /** Authenticated report of the endpoint used by this worker, for coordinated moves. */
  controllerUrl: z.string().max(2048).optional(),
  platform: z.string().max(32),
  osRelease: z.string().max(255),
  arch: z.string().max(32),
  cpus: z.number().int().positive(),
  memoryTotalMb: z.number().int().positive(),
  agentVersion: z.string().max(64),
  docker: ToolStatusSchema,
  buildx: ToolStatusSchema,
  act: ToolStatusSchema,
  /** Executor kinds this agent can run, e.g. ["command"]. */
  executors: z.array(z.string()).max(32),
  /** The exact build the agent runs; absent for agents older than self-update. */
  build: AgentBuildSchema.optional(),
});
export type NodeInfo = z.infer<typeof NodeInfoSchema>;

export const NodeMetricsSchema = z.object({
  ts: TimestampSchema,
  cpuPercent: z.number().min(0).max(100),
  memoryAvailableMb: z.number().int().nonnegative(),
  diskFreeMb: z.number().int().nonnegative(),
  diskTotalMb: z.number().int().nonnegative(),
  load1: z.number().nonnegative(),
});
export type NodeMetrics = z.infer<typeof NodeMetricsSchema>;

export const SchedulingModeSchema = z.enum(['enabled', 'disabled', 'draining']);
export type SchedulingMode = z.infer<typeof SchedulingModeSchema>;

export const NodeSettingsSchema = z.object({
  maxConcurrency: z.number().int().min(1).max(256),
  /** Resources held back for the host (e.g. interactive development); never allocated to jobs. */
  reserved: z.object({
    cpus: z.number().min(0).max(1024),
    memoryMb: z.number().int().min(0),
  }),
  labels: z.array(LabelSchema).max(64),
  schedulingMode: SchedulingModeSchema,
  autoUpdate: NodeAutoUpdateSchema.default('inherit'),
});
export type NodeSettings = z.infer<typeof NodeSettingsSchema>;

export const NodeSettingsPatchSchema = z.object({
  maxConcurrency: NodeSettingsSchema.shape.maxConcurrency.optional(),
  reserved: z
    .object({
      cpus: z.number().min(0).max(1024).optional(),
      memoryMb: z.number().int().min(0).optional(),
    })
    .optional(),
  labels: NodeSettingsSchema.shape.labels.optional(),
  schedulingMode: SchedulingModeSchema.optional(),
  autoUpdate: NodeAutoUpdateSchema.optional(),
});
export type NodeSettingsPatch = z.infer<typeof NodeSettingsPatchSchema>;

export function defaultNodeSettings(info: Pick<NodeInfo, 'cpus' | 'memoryTotalMb'>): NodeSettings {
  return {
    maxConcurrency: Math.max(1, Math.min(4, Math.floor(info.cpus / 2))),
    reserved: {
      cpus: info.cpus >= 4 ? 1 : 0,
      memoryMb: Math.min(4096, Math.floor(info.memoryTotalMb * 0.1)),
    },
    labels: [],
    schedulingMode: 'enabled',
    autoUpdate: 'inherit',
  };
}

export const NodeStatusSchema = z.enum(['online', 'offline']);
export type NodeStatus = z.infer<typeof NodeStatusSchema>;

export const NodeSchema = z.object({
  id: z.string(),
  name: z.string(),
  status: NodeStatusSchema,
  health: z.enum(['ok', 'degraded', 'unknown']),
  healthIssues: z.array(z.string()),
  info: NodeInfoSchema.nullable(),
  metrics: NodeMetricsSchema.nullable(),
  settings: NodeSettingsSchema,
  /** Automatic labels (os=, arch=, capability names) plus settings.labels. */
  effectiveLabels: z.array(z.string()),
  activeJobs: z.array(z.string()),
  allocated: z.object({ cpus: z.number(), memoryMb: z.number().int() }),
  allocatable: z.object({ cpus: z.number(), memoryMb: z.number().int() }),
  lastSeenAt: TimestampSchema.nullable(),
  connectedAt: TimestampSchema.nullable(),
  createdAt: TimestampSchema,
  /** Optional so newer clients can read older controllers. */
  update: NodeUpdateStatusSchema.optional(),
});
export type Node = z.infer<typeof NodeSchema>;

/** Labels every node gets automatically, so jobs can target `os=linux`, `arch=arm64`, `docker`. */
export function automaticLabels(info: NodeInfo | null): string[] {
  if (!info) return [];
  const labels = [`os=${info.platform}`, `arch=${info.arch}`];
  if (info.docker.available) labels.push('docker');
  if (info.buildx.available) labels.push('buildx');
  if (info.act.available) labels.push('act');
  return labels;
}

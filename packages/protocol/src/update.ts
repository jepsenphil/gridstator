import { z } from 'zod';
import { TimestampSchema } from './common.js';
import { AgentToolsSchema, ToolNameSchema, ToolPlatformSchema } from './toolchain.js';

/**
 * Worker updates. The controller is the source of truth: a worker is current when it runs the
 * controller's exact build (same version and entry-point sha256). Workers fetch that build from
 * the controller over the update endpoint below, install it into their version store and restart.
 */

/** A gridstator build: `<version>-<sha12>`, or `<version>-unbundled` for a workspace build. */
export const BuildRefSchema = z.object({
  id: z.string().min(1).max(128),
  version: z.string().min(1).max(64),
  /** sha256 of the bundled entry point; null for the unbundled workspace build. */
  sha256: z
    .string()
    .regex(/^[0-9a-f]{64}$/)
    .nullable(),
});
export type BuildRef = z.infer<typeof BuildRefSchema>;

export const RollbackSchema = z.object({
  from: z.string().max(128),
  to: z.string().max(128),
  at: TimestampSchema,
  reason: z.enum(['crashed', 'timed-out']),
});

/** What an agent reports about the build it runs (part of NodeInfo). */
export const AgentBuildSchema = BuildRefSchema.extend({
  /** Started from a managed install (version store + launcher), so it can update itself. */
  managed: z.boolean(),
  /** Node.js version, e.g. "v24.11.0". */
  node: z.string().max(32),
  /** The launcher's last automatic rollback, if any. */
  lastRollback: RollbackSchema.nullable(),
  /** Versions of the managed tools it runs; absent before toolchain management. */
  tools: AgentToolsSchema.optional(),
});
export type AgentBuild = z.infer<typeof AgentBuildSchema>;

export const ControllerBuildSchema = BuildRefSchema.extend({
  /** Workers can download this build from the controller. */
  updatable: z.boolean(),
  /** Why workers cannot update to it (e.g. the controller runs an unbundled build). */
  reason: z.string().nullable(),
  /** Distinct from worker package availability: the controller can install and restart itself. */
  selfUpdate: z.object({ available: z.boolean(), reason: z.string().nullable() }).optional(),
});
export type ControllerBuild = z.infer<typeof ControllerBuildSchema>;

export const AutoUpdateModeSchema = z.enum(['off', 'follow', 'window']);
export type AutoUpdateMode = z.infer<typeof AutoUpdateModeSchema>;

/** Per-node override; `inherit` uses the cluster default. */
export const NodeAutoUpdateSchema = z.enum(['inherit', 'off', 'follow', 'window']);
export type NodeAutoUpdate = z.infer<typeof NodeAutoUpdateSchema>;

const ClockTimeSchema = z.string().regex(/^([01]\d|2[0-3]):[0-5]\d$/, 'expected HH:MM (24-hour)');

/** Daily window in the controller's local time; `end` before `start` wraps past midnight. */
export const UpdateWindowSchema = z
  .object({ start: ClockTimeSchema, end: ClockTimeSchema })
  .refine((w) => w.start !== w.end, 'the window must not be empty');
export type UpdateWindow = z.infer<typeof UpdateWindowSchema>;

/** Whether `now` falls inside the daily window (local time). */
export function inUpdateWindow(window: UpdateWindow, now: Date = new Date()): boolean {
  const minutes = (t: string) => Number(t.slice(0, 2)) * 60 + Number(t.slice(3, 5));
  const m = now.getHours() * 60 + now.getMinutes();
  const start = minutes(window.start);
  const end = minutes(window.end);
  return start < end ? m >= start && m < end : m >= start || m < end;
}

/**
 * - current: runs the controller's build
 * - behind: runs another build (online, or connected only to update after a protocol change)
 * - updating: taking part in a rollout right now (draining or restarting)
 * - failed: the last update to the controller's build failed; not retried automatically
 * - unknown: offline, so the running build cannot be confirmed
 */
export const NodeUpdateStateSchema = z.enum(['current', 'behind', 'updating', 'failed', 'unknown']);
export type NodeUpdateState = z.infer<typeof NodeUpdateStateSchema>;

export const NodeUpdateStatusSchema = z.object({
  state: NodeUpdateStateSchema,
  /** The build the node last reported. */
  build: z.string().nullable(),
  detail: z.string().nullable(),
  /** Effective auto-update mode (the node override, else the cluster default). */
  autoUpdate: AutoUpdateModeSchema,
  /** The node can update itself (managed install, recent enough agent). */
  canUpdate: z.boolean(),
});
export type NodeUpdateStatus = z.infer<typeof NodeUpdateStatusSchema>;

export const RolloutNodeStateSchema = z.enum([
  'pending',
  'draining',
  'updating',
  'done',
  'failed',
  'skipped',
]);
export type RolloutNodeState = z.infer<typeof RolloutNodeStateSchema>;

export const RolloutNodeSchema = z.object({
  nodeId: z.string(),
  name: z.string(),
  state: RolloutNodeStateSchema,
  detail: z.string().nullable(),
  /** Added by auto-update (follows the node's mode and window) rather than by an operator. */
  auto: z.boolean(),
  startedAt: TimestampSchema.nullable(),
  finishedAt: TimestampSchema.nullable(),
});
export type RolloutNode = z.infer<typeof RolloutNodeSchema>;

export const RolloutSchema = z.object({
  id: z.string(),
  target: BuildRefSchema,
  state: z.enum(['running', 'halted', 'finished']),
  haltReason: z.string().nullable(),
  startedBy: z.string(),
  startedAt: TimestampSchema,
  finishedAt: TimestampSchema.nullable(),
  /** Skip draining: cancel running jobs and update straight away. */
  now: z.boolean(),
  /** Explicit cancellation deadline; null waits for running jobs without cancelling them. */
  drainTimeoutSeconds: z.number().int().positive().nullable(),
  nodes: z.array(RolloutNodeSchema),
});
export type Rollout = z.infer<typeof RolloutSchema>;

/** A managed tool as the controller sees it. */
export const ToolchainEntrySchema = z.object({
  name: ToolNameSchema,
  /** null when management is turned off. */
  version: z.string().nullable(),
  source: z.enum(['pinned', 'override', 'off']),
  /** Mirror state per platform that workers need. */
  platforms: z.array(
    z.object({
      platform: ToolPlatformSchema,
      state: z.enum(['ready', 'preparing', 'failed']),
      reason: z.string().nullable(),
    }),
  ),
});
export type ToolchainEntry = z.infer<typeof ToolchainEntrySchema>;

export const UpdatesViewSchema = z.object({
  controller: ControllerBuildSchema,
  autoUpdate: AutoUpdateModeSchema,
  updateWindow: UpdateWindowSchema.nullable(),
  rollout: RolloutSchema.nullable(),
  /** Optional so newer clients can read older controllers. */
  toolchain: z.array(ToolchainEntrySchema).optional(),
});
export type UpdatesView = z.infer<typeof UpdatesViewSchema>;

export const StartUpdateRequestSchema = z.object({
  /** Node names or ids; default: every online node that is not current. */
  nodes: z.array(z.string()).max(1000).optional(),
  now: z.boolean().default(false),
  drainTimeoutSeconds: z
    .number()
    .int()
    .positive()
    .max(7 * 24 * 3600)
    .nullable()
    .default(null),
});
export type StartUpdateRequest = z.input<typeof StartUpdateRequestSchema>;

// ---- update endpoint --------------------------------------------------------------------------
//
// Agents of any version use these three routes to update, including agents whose agent-channel
// protocol no longer matches the controller's. Their shape must never change; add new routes
// instead.

export const AGENT_UPDATE_PATH = '/v1/agent/update';

export const AgentUpdateCheckQuerySchema = z.object({
  /** The build the agent runs. */
  current: z.string().max(128),
  /** `update-only`: the agent could not join over the agent channel (protocol mismatch). */
  mode: z.enum(['normal', 'update-only']).default('normal'),
});

export const AgentUpdateCheckResponseSchema = z.discriminatedUnion('action', [
  z.object({ action: z.literal('none'), reason: z.string().nullable() }),
  z.object({
    action: z.literal('update'),
    build: BuildRefSchema,
    /** Download from `${AGENT_UPDATE_PATH}/package/${build.id}`. */
    package: z.object({
      sha256: z.string().regex(/^[0-9a-f]{64}$/),
      size: z.number().int().positive(),
    }),
    /** The new version must reconnect within this long, or the launcher rolls it back. */
    confirmWithinSeconds: z.number().int().positive(),
  }),
]);
export type AgentUpdateCheckResponse = z.infer<typeof AgentUpdateCheckResponseSchema>;

/** `POST /v1/controller/build` response. */
export const PublishBuildResponseSchema = z.object({
  action: z.enum(['none', 'restarting']),
  from: z.string(),
  to: z.string(),
});
export type PublishBuildResponse = z.infer<typeof PublishBuildResponseSchema>;

export const AgentUpdateReportSchema = z.object({
  target: z.string().max(128),
  error: z.string().max(4096),
});
export type AgentUpdateReport = z.infer<typeof AgentUpdateReportSchema>;

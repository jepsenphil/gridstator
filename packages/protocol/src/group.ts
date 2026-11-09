import { z } from 'zod';
import { IdSchema, TimestampSchema } from './common.js';
import { JobSchema, JobSpecSchema, type JobSpec } from './job.js';
import { JobStateSchema } from './states.js';

export const MAX_SHARDS = 64;

export const SubmitGroupRequestSchema = z.object({
  name: z.string().min(1).max(200).optional(),
  /** A command job spec; `{shard}` and `{total}` in its command are replaced per shard. */
  spec: JobSpecSchema,
  shards: z.number().int().min(1).max(MAX_SHARDS),
});
export type SubmitGroupRequest = z.input<typeof SubmitGroupRequestSchema>;

export const GroupStateSchema = z.enum(['queued', 'running', 'succeeded', 'failed', 'cancelled']);
export type GroupState = z.infer<typeof GroupStateSchema>;

export const JobGroupSchema = z.object({
  id: IdSchema,
  name: z.string(),
  owner: z.string(),
  shardTotal: z.number().int().positive(),
  state: GroupStateSchema,
  /** Latest attempt of each shard, ordered by shard index. */
  jobs: z.array(JobSchema),
  counts: z.partialRecord(JobStateSchema, z.number().int()),
  nodes: z.array(z.string()),
  createdAt: TimestampSchema,
  startedAt: TimestampSchema.nullable(),
  finishedAt: TimestampSchema.nullable(),
});
export type JobGroup = z.infer<typeof JobGroupSchema>;

export const ListGroupsResponseSchema = z.object({ groups: z.array(JobGroupSchema) });

/** Environment variables every shard receives. */
export function shardEnv(index: number, total: number): Record<string, string> {
  return { GRIDSTATOR_SHARD_INDEX: String(index), GRIDSTATOR_SHARD_TOTAL: String(total) };
}

/** Applies shard placeholders (`{shard}`, `{total}`) and variables to a command spec. */
export function shardSpec(spec: JobSpec, index: number, total: number): JobSpec {
  if (spec.kind !== 'command') throw new Error('Only command jobs can be sharded');
  const fill = (s: string) =>
    s.replaceAll('{shard}', String(index)).replaceAll('{total}', String(total));
  return {
    ...spec,
    name: spec.name
      ? `${fill(spec.name)}${spec.name.includes('{shard}') ? '' : ` [${index}/${total}]`}`
      : undefined,
    command: spec.command.map(fill),
    env: { ...spec.env, ...shardEnv(index, total) },
  };
}

/** Aggregate state of a group from the latest attempt of each shard. */
export function groupState(states: readonly z.infer<typeof JobStateSchema>[]): GroupState {
  const terminal = (s: string) =>
    s === 'succeeded' || s === 'failed' || s === 'cancelled' || s === 'lost';
  if (states.length > 0 && states.every(terminal)) {
    if (states.some((s) => s === 'failed' || s === 'lost')) return 'failed';
    if (states.some((s) => s === 'cancelled')) return 'cancelled';
    return 'succeeded';
  }
  if (states.some((s) => s === 'assigned' || s === 'starting' || s === 'running' || terminal(s)))
    return 'running';
  return 'queued';
}

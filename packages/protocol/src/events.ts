import { z } from 'zod';
import { JobSchema, LogEntrySchema } from './job.js';
import { NodeSchema } from './node.js';
import { JobStateSchema } from './states.js';

// ---- client → controller ------------------------------------------------------------------

export const ClientSubscribeSchema = z.object({
  type: z.literal('subscribe'),
  topics: z.array(z.enum(['jobs', 'nodes'])),
});

export const ClientLogsSubscribeSchema = z.object({
  type: z.literal('logs.subscribe'),
  jobId: z.string(),
  /** Deliver entries with sequence numbers greater than this. */
  afterSeq: z.number().int().min(0).default(0),
});

export const ClientLogsUnsubscribeSchema = z.object({
  type: z.literal('logs.unsubscribe'),
  jobId: z.string(),
});

export const ClientEventMessageSchema = z.discriminatedUnion('type', [
  ClientSubscribeSchema,
  ClientLogsSubscribeSchema,
  ClientLogsUnsubscribeSchema,
]);
export type ClientEventMessage = z.input<typeof ClientEventMessageSchema>;

// ---- controller → client ------------------------------------------------------------------

export const ServerJobUpdatedSchema = z.object({ type: z.literal('job.updated'), job: JobSchema });
export const ServerNodeUpdatedSchema = z.object({
  type: z.literal('node.updated'),
  node: NodeSchema,
});
export const ServerNodeRemovedSchema = z.object({
  type: z.literal('node.removed'),
  nodeId: z.string(),
});
export const ServerLogsSchema = z.object({
  type: z.literal('logs'),
  jobId: z.string(),
  entries: z.array(LogEntrySchema),
});
/** Sent after the final log entry of a terminal job has been delivered. */
export const ServerLogsEndSchema = z.object({
  type: z.literal('logs.end'),
  jobId: z.string(),
  state: JobStateSchema,
});
export const ServerErrorSchema = z.object({ type: z.literal('error'), message: z.string() });

export const ServerEventMessageSchema = z.discriminatedUnion('type', [
  ServerJobUpdatedSchema,
  ServerNodeUpdatedSchema,
  ServerNodeRemovedSchema,
  ServerLogsSchema,
  ServerLogsEndSchema,
  ServerErrorSchema,
]);
export type ServerEventMessage = z.infer<typeof ServerEventMessageSchema>;

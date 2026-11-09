import { z } from 'zod';

export const JOB_STATES = [
  'created',
  'queued',
  'assigned',
  'starting',
  'running',
  'succeeded',
  'failed',
  'cancelled',
  'lost',
] as const;

export const JobStateSchema = z.enum(JOB_STATES);
export type JobState = z.infer<typeof JobStateSchema>;

export const TERMINAL_STATES: ReadonlySet<JobState> = new Set([
  'succeeded',
  'failed',
  'cancelled',
  'lost',
]);

/** States in which a job holds a reservation on a node. */
export const DISPATCHED_STATES: ReadonlySet<JobState> = new Set([
  'assigned',
  'starting',
  'running',
]);

/**
 * The single definition of legal job state transitions (see docs/ARCHITECTURE.md §4).
 * `assigned` may jump forward because agent reports can arrive after a reconnect in which the
 * intermediate report was already superseded.
 */
const TRANSITIONS: Readonly<Record<JobState, readonly JobState[]>> = {
  created: ['queued', 'cancelled', 'failed'],
  queued: ['assigned', 'cancelled', 'failed'],
  assigned: ['queued', 'starting', 'running', 'succeeded', 'failed', 'cancelled', 'lost'],
  starting: ['running', 'succeeded', 'failed', 'cancelled', 'lost'],
  running: ['succeeded', 'failed', 'cancelled', 'lost'],
  succeeded: [],
  failed: [],
  cancelled: [],
  lost: [],
};

export function isTerminal(state: JobState): boolean {
  return TERMINAL_STATES.has(state);
}

export function canTransition(from: JobState, to: JobState): boolean {
  return TRANSITIONS[from].includes(to);
}

export function allowedTransitions(from: JobState): readonly JobState[] {
  return TRANSITIONS[from];
}

import type { Readable } from 'node:stream';
import type { JobSpec, LogStream } from '@gridstator/protocol';

export interface ExecutionContext {
  jobId: string;
  assignmentId: string;
  nodeId: string;
  /** Human-readable node name for messages. */
  nodeName: string;
  attempt: number;
  spec: JobSpec;
  /** Produces a tar of the job's source snapshot, or null for jobs without a snapshot. */
  workspaceTar: (() => Readable) | null;
  /** Raw output from the job; the caller handles decoding, masking and batching. */
  output(stream: Exclude<LogStream, 'system'>, chunk: Buffer): void;
  /** Gridstator's own progress messages, shown in the log as system lines. */
  system(message: string): void;
  /** Called once the user's process has started. */
  onRunning(): void;
  /** Aborted when the job is cancelled. */
  signal: AbortSignal;
}

export interface ExecutionResult {
  outcome: 'succeeded' | 'failed' | 'cancelled';
  exitCode: number | null;
  error: string | null;
  oomKilled: boolean;
}

export interface Executor {
  readonly kind: JobSpec['kind'];
  run(ctx: ExecutionContext): Promise<ExecutionResult>;
}

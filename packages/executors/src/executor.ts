import { realpath, stat } from 'node:fs/promises';
import { join, sep } from 'node:path';
import type { Readable } from 'node:stream';
import type { Artifact, JobOutputs, JobSpec, LogStream } from '@gridstator/protocol';

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
  /**
   * Writes the snapshot's working tree into `dest` (which must not exist or be empty) using the
   * hardened materializer. Null for jobs without a snapshot.
   */
  materialize: ((dest: string) => Promise<void>) | null;
  /** Private per-job directory on the worker; created before `run` and removed afterwards. */
  workDir: string;
  /** Raw output from the job; the caller handles decoding, masking and batching. */
  output(stream: Exclude<LogStream, 'system'>, chunk: Buffer): void;
  /** Gridstator's own progress messages, shown in the log as system lines. */
  system(message: string): void;
  /** Called once the user's process has started. */
  onRunning(): void;
  /** Uploads a local file to the controller as a job artifact. */
  uploadArtifact(name: string, filePath: string): Promise<Artifact>;
  /** Publishes structured progress (replaces earlier outputs). */
  reportOutputs(outputs: JobOutputs): void;
  /** Aborted when the job is cancelled. */
  signal: AbortSignal;
}

export interface ExecutionResult {
  outcome: 'succeeded' | 'failed' | 'cancelled';
  exitCode: number | null;
  error: string | null;
  oomKilled: boolean;
  outputs?: JobOutputs;
  artifacts?: Artifact[];
}

export interface Executor {
  readonly kind: JobSpec['kind'];
  run(ctx: ExecutionContext): Promise<ExecutionResult>;
}

export function failed(error: string, exitCode: number | null = null): ExecutionResult {
  return { outcome: 'failed', exitCode, error, oomKilled: false };
}

export function cancelled(): ExecutionResult {
  return { outcome: 'cancelled', exitCode: null, error: null, oomKilled: false };
}

/**
 * Resolves a snapshot-relative path inside `root`, following symlinks, and verifies the result is
 * still inside `root` and of the expected type. Returns undefined when it does not exist.
 */
export async function resolveInside(
  root: string,
  rel: string,
  type: 'file' | 'directory',
): Promise<{ path: string } | { error: string }> {
  const realRoot = await realpath(root);
  let resolved: string;
  try {
    resolved = await realpath(rel === '.' ? root : join(root, rel));
  } catch {
    return { error: `"${rel}" does not exist in the snapshot` };
  }
  if (resolved !== realRoot && !resolved.startsWith(realRoot + sep)) {
    return { error: `"${rel}" resolves outside the snapshot` };
  }
  const s = await stat(resolved);
  if (type === 'file' ? !s.isFile() : !s.isDirectory()) {
    return { error: `"${rel}" is not a ${type}` };
  }
  return { path: resolved };
}

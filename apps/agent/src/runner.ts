import { join } from 'node:path';
import type { Logger } from '@gridstator/shared';
import { errorMessage } from '@gridstator/shared';
import {
  parseManifest,
  type AgentJobResult,
  type ControllerJobAssign,
  type LogEntry,
  type Manifest,
} from '@gridstator/protocol';
import type { GridstatorClient } from '@gridstator/client';
import type { ExecutionResult, Executor } from '@gridstator/executors';
import { unpackBlobsInto, workspaceTar, type BlobStore } from '@gridstator/snapshot';
import { LogPipeline } from './log-pipeline.js';

const DOWNLOAD_BATCH = 5000;

/** Ensures a snapshot's manifest and every blob it references are in the local cache. */
export async function fetchSnapshot(
  client: GridstatorClient,
  cache: BlobStore,
  snapshotId: string,
  report: (msg: string) => void,
): Promise<Manifest> {
  if (!(await cache.has(snapshotId))) {
    await cache.putBuffer(snapshotId, await client.getBlob(snapshotId));
  }
  const manifest = parseManifest(await cache.readBuffer(snapshotId));
  const digests = [
    ...new Set(manifest.entries.flatMap((e) => (e.type === 'file' ? [e.digest] : []))),
  ];
  const missing = await cache.missing(digests);
  const totalBytes = manifest.entries.reduce((n, e) => n + (e.type === 'file' ? e.size : 0), 0);
  report(
    `Snapshot ${snapshotId.slice(7, 19)}: ${manifest.entries.length} entries, ${(totalBytes / 1024 / 1024).toFixed(1)} MiB` +
      (missing.length === 0
        ? ' (fully cached)'
        : `, fetching ${missing.length} of ${digests.length} blobs`),
  );
  for (let i = 0; i < missing.length; i += DOWNLOAD_BATCH) {
    const res = await client.downloadBlobs(missing.slice(i, i + DOWNLOAD_BATCH));
    await unpackBlobsInto(res, cache);
  }
  // Refresh LRU timestamps without blocking the job on thousands of utimes calls.
  void Promise.all(digests.slice(0, 20_000).map((d) => cache.touch(d)));
  return manifest;
}

export interface RunnerDeps {
  client: GridstatorClient;
  cache: BlobStore;
  executor: Executor;
  nodeId: string;
  nodeName: string;
  logsDir: string;
  maxLogBytes: number;
  logger: Logger;
  /** Sends a batch of log entries if the controller channel is ready. */
  sendLogs: (runner: JobRunner, entries: LogEntry[]) => void;
  sendPhase: (runner: JobRunner, phase: 'starting' | 'running') => void;
  /** Persists and sends the terminal result. */
  complete: (runner: JobRunner, result: AgentJobResult) => void;
}

/** Runs one assignment end-to-end on this agent. */
export class JobRunner {
  readonly jobId: string;
  readonly assignmentId: string;
  readonly spoolPath: string;
  phase: 'starting' | 'running' = 'starting';
  manifest: Manifest | null = null;
  private readonly assign: ControllerJobAssign;
  private readonly deps: RunnerDeps;
  private readonly abort = new AbortController();
  private abandonReason: string | undefined;
  readonly pipeline: LogPipeline;
  readonly done: Promise<void>;
  private resolveDone!: () => void;

  constructor(assign: ControllerJobAssign, deps: RunnerDeps) {
    this.assign = assign;
    this.deps = deps;
    this.jobId = assign.jobId;
    this.assignmentId = assign.assignmentId;
    this.spoolPath = join(deps.logsDir, `${assign.jobId}.ndjson`);
    this.pipeline = new LogPipeline({
      spoolPath: this.spoolPath,
      secrets: Object.values(assign.spec.secretEnv),
      maxBytes: deps.maxLogBytes,
      sink: (entries) => deps.sendLogs(this, entries),
    });
    this.done = new Promise((r) => (this.resolveDone = r));
  }

  get logSeq(): number {
    return this.pipeline.lastSeq;
  }

  /** Cancels on behalf of the user. */
  cancel(reason: string): void {
    if (this.abort.signal.aborted) return;
    this.pipeline.system(`Cancellation requested: ${reason}`);
    this.abort.abort();
  }

  /** Stops the job because the agent is going away; reported as lost, not as a user failure. */
  abandon(reason: string): void {
    this.abandonReason = reason;
    this.cancel(reason);
  }

  start(): void {
    void this.run().finally(() => this.resolveDone());
  }

  private async run(): Promise<void> {
    const { spec } = this.assign;
    const log = this.deps.logger.child({ jobId: this.jobId });
    let result: ExecutionResult;
    this.deps.sendPhase(this, 'starting');
    try {
      this.pipeline.system(
        `Job ${this.jobId} (attempt ${this.assign.attempt}) accepted by node ${this.deps.nodeName}`,
      );
      if (this.assign.snapshotId) {
        this.manifest = await fetchSnapshot(
          this.deps.client,
          this.deps.cache,
          this.assign.snapshotId,
          (m) => this.pipeline.system(m),
        );
      }
      const manifest = this.manifest;
      result = await this.deps.executor.run({
        jobId: this.jobId,
        assignmentId: this.assignmentId,
        nodeId: this.deps.nodeId,
        nodeName: this.deps.nodeName,
        attempt: this.assign.attempt,
        spec,
        workspaceTar: manifest ? () => workspaceTar(manifest, this.deps.cache) : null,
        output: (stream, chunk) => this.pipeline.output(stream, chunk),
        system: (m) => this.pipeline.system(m),
        onRunning: () => {
          this.phase = 'running';
          this.deps.sendPhase(this, 'running');
        },
        signal: this.abort.signal,
      });
    } catch (err) {
      log.error({ err }, 'job setup failed');
      result = { outcome: 'failed', exitCode: null, error: errorMessage(err), oomKilled: false };
    }
    const outcome: AgentJobResult['outcome'] =
      this.abandonReason && result.outcome !== 'succeeded' ? 'lost' : result.outcome;
    const error = outcome === 'lost' ? (this.abandonReason ?? null) : result.error;
    const summary =
      outcome === 'succeeded'
        ? 'Job succeeded'
        : outcome === 'cancelled'
          ? 'Job cancelled'
          : outcome === 'lost'
            ? `Job abandoned: ${error}`
            : `Job failed: ${error ?? `exit code ${result.exitCode}`}`;
    this.pipeline.system(summary);
    this.pipeline.finish();
    this.deps.complete(this, {
      jobId: this.jobId,
      assignmentId: this.assignmentId,
      outcome,
      exitCode: result.exitCode,
      error,
      oomKilled: result.oomKilled,
      finishedAt: Date.now(),
      logSeq: this.pipeline.lastSeq,
    });
    log.info({ outcome, exitCode: result.exitCode }, 'job finished');
  }
}

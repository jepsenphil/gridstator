import { readdir } from 'node:fs/promises';
import type { Logger } from '@gridstator/shared';
import { isTerminal, parseManifest, type ClusterSettings } from '@gridstator/protocol';
import type { BlobStore } from '@gridstator/snapshot';
import type { Store } from './db.js';
import type { LogStore } from './logs.js';

const DAY = 24 * 3600_000;
/** Blobs younger than this are never collected (an upload may not be registered yet). */
const BLOB_GRACE_MS = 6 * 3600_000;

export interface RetentionResult {
  jobsDeleted: number;
  logsDeleted: number;
  snapshotsDeleted: number;
  blobsDeleted: number;
  bytesFreed: number;
}

/** Applies the cluster retention policy (ARCHITECTURE §7). Safe to run at any time. */
export async function runRetention(deps: {
  store: Store;
  logs: LogStore;
  logsDir: string;
  blobs: BlobStore;
  settings: ClusterSettings;
  logger: Logger;
  now?: number;
}): Promise<RetentionResult> {
  const now = deps.now ?? Date.now();
  const result: RetentionResult = {
    jobsDeleted: 0,
    logsDeleted: 0,
    snapshotsDeleted: 0,
    blobsDeleted: 0,
    bytesFreed: 0,
  };

  for (;;) {
    const old = deps.store.terminalJobsFinishedBefore(
      now - deps.settings.jobRetentionDays * DAY,
      500,
    );
    if (old.length === 0) break;
    for (const { id } of old) {
      deps.store.deleteJob(id);
      deps.logs.delete(id);
      result.jobsDeleted++;
    }
  }

  deps.store.deleteEmptyGroups();

  const logCutoff = now - deps.settings.logRetentionDays * DAY;
  for (const file of await readdir(deps.logsDir).catch(() => [] as string[])) {
    const match = /^([0-9A-Z]{26})\.ndjson$/.exec(file);
    if (!match) continue;
    const job = deps.store.jobById(match[1]!);
    if (!job || (isTerminal(job.state) && (job.finished_at ?? 0) < logCutoff)) {
      deps.logs.delete(match[1]!);
      result.logsDeleted++;
    }
  }

  for (const id of deps.store.staleSnapshots(now - deps.settings.snapshotRetentionDays * DAY)) {
    deps.store.deleteSnapshot(id);
    result.snapshotsDeleted++;
  }

  // Mark-and-sweep: keep every blob referenced by a remaining snapshot.
  const live = new Set<string>(deps.store.artifactDigests());
  for (const id of deps.store.listSnapshotIds()) {
    live.add(id);
    try {
      for (const e of parseManifest(await deps.blobs.readBuffer(id)).entries) {
        if (e.type === 'file') live.add(e.digest);
      }
    } catch (err) {
      deps.logger.warn(
        { snapshot: id, err },
        'snapshot manifest unreadable; deleting snapshot record',
      );
      deps.store.deleteSnapshot(id);
    }
  }
  for await (const blob of deps.blobs.list()) {
    if (live.has(blob.digest) || now - blob.mtimeMs < BLOB_GRACE_MS) continue;
    await deps.blobs.remove(blob.digest);
    result.blobsDeleted++;
    result.bytesFreed += blob.size;
  }
  return result;
}

import { createReadStream } from 'node:fs';
import { Readable } from 'node:stream';
import type { BlobsUploadResponse, SnapshotView } from '@gridstator/protocol';
import type { CapturedSnapshot } from './capture.js';
import { packBlobs, type BlobSource } from './tar.js';

/** The subset of the controller API needed to upload a snapshot. */
export interface SnapshotTransport {
  missingBlobs(digests: string[]): Promise<string[]>;
  uploadBlobs(body: Readable): Promise<BlobsUploadResponse>;
  createSnapshot(manifestDigest: string): Promise<SnapshotView>;
}

export interface UploadProgress {
  missing: number;
  missingBytes: number;
  uploadedBlobs: number;
  uploadedBytes: number;
}

const BATCH_BYTES = 256 * 1024 * 1024;
const BATCH_COUNT = 20_000;

/**
 * Uploads the blobs the controller lacks (in bounded batches), then registers the snapshot. The
 * controller verifies every blob, so content that changed since capture fails loudly instead of
 * producing a snapshot that does not match its ID.
 */
export async function uploadSnapshot(
  captured: CapturedSnapshot,
  transport: SnapshotTransport,
  onProgress?: (p: UploadProgress) => void,
): Promise<{ snapshot: SnapshotView; progress: UploadProgress }> {
  const all = [captured.id, ...captured.blobs.keys()];
  const missing = await transport.missingBlobs(all);
  const sources: BlobSource[] = missing.map((digest) => {
    if (digest === captured.id) {
      const bytes = captured.manifestBytes;
      return {
        digest,
        size: bytes.length,
        open: () => Readable.from([bytes]),
      };
    }
    const file = captured.blobs.get(digest);
    if (!file) throw new Error(`Controller asked for unknown blob ${digest}`);
    return { digest, size: file.size, open: () => createReadStream(file.absPath) };
  });
  const progress: UploadProgress = {
    missing: sources.length,
    missingBytes: sources.reduce((n, s) => n + s.size, 0),
    uploadedBlobs: 0,
    uploadedBytes: 0,
  };
  onProgress?.(progress);
  let batch: BlobSource[] = [];
  let batchBytes = 0;
  const flush = async () => {
    if (batch.length === 0) return;
    await transport.uploadBlobs(packBlobs(batch));
    progress.uploadedBlobs += batch.length;
    progress.uploadedBytes += batchBytes;
    onProgress?.(progress);
    batch = [];
    batchBytes = 0;
  };
  for (const source of sources) {
    batch.push(source);
    batchBytes += source.size;
    if (batchBytes >= BATCH_BYTES || batch.length >= BATCH_COUNT) await flush();
  }
  await flush();
  const snapshot = await transport.createSnapshot(captured.id);
  return { snapshot, progress };
}

import { once } from 'node:events';
import { Readable } from 'node:stream';
import tar from 'tar-stream';
import { GridstatorError } from '@gridstator/shared';
import type { Manifest } from '@gridstator/protocol';
import type { BlobStore } from './cas.js';

export interface BlobSource {
  digest: string;
  size: number;
  open(): AsyncIterable<Uint8Array>;
}

const BLOB_ENTRY = /^sha256\/([0-9a-f]{64})$/;

interface EntryWritable {
  write(chunk: Uint8Array): boolean;
  end(): void;
  on(event: string, listener: (...args: unknown[]) => void): unknown;
}

/** Writes an async iterable into a tar-stream entry, honouring backpressure. */
async function writeEntry(
  entry: EntryWritable,
  done: Promise<void>,
  source: AsyncIterable<Uint8Array>,
): Promise<void> {
  for await (const chunk of source) {
    if (!entry.write(chunk)) await once(entry as unknown as NodeJS.EventEmitter, 'drain');
  }
  entry.end();
  await done;
}

function addEntry(
  pack: tar.Pack,
  header: Partial<tar.Header> & { name: string },
  source: AsyncIterable<Uint8Array>,
): Promise<void> {
  let resolveDone!: () => void;
  let rejectDone!: (err: Error) => void;
  const done = new Promise<void>((resolve, reject) => {
    resolveDone = resolve;
    rejectDone = reject;
  });
  const entry = pack.entry(header, (err) => (err ? rejectDone(err) : resolveDone()));
  return writeEntry(entry as unknown as EntryWritable, done, source);
}

/** Packs blobs into a tar stream (Node Readable) whose entries are named `sha256/<hex>`. */
export function packBlobs(blobs: Iterable<BlobSource> | AsyncIterable<BlobSource>): Readable {
  const pack = tar.pack();
  void (async () => {
    try {
      for await (const blob of blobs) {
        const name = `sha256/${blob.digest.slice('sha256:'.length)}`;
        await addEntry(pack, { name, size: blob.size, mode: 0o600 }, blob.open());
      }
      pack.finalize();
    } catch (err) {
      pack.destroy(err as Error);
    }
  })();
  return Readable.from(pack as AsyncIterable<Buffer>, { objectMode: false });
}

/**
 * Reads a blob tar stream (as produced by packBlobs) into a BlobStore, verifying every digest.
 * Rejects unexpected entry names and enforces optional limits.
 */
export async function unpackBlobsInto(
  source: AsyncIterable<Uint8Array>,
  store: BlobStore,
  limits: { maxEntries?: number; maxBytes?: number } = {},
): Promise<{ stored: number; alreadyPresent: number; bytes: number }> {
  const extract = tar.extract();
  void (async () => {
    try {
      for await (const chunk of source) {
        if (!extract.write(chunk)) await once(extract as unknown as NodeJS.EventEmitter, 'drain');
      }
      extract.end(undefined);
    } catch (err) {
      extract.destroy(err as Error);
    }
  })();
  let stored = 0;
  let alreadyPresent = 0;
  let bytes = 0;
  let entries = 0;
  for await (const entry of extract) {
    const match = BLOB_ENTRY.exec(entry.header.name);
    if (!match || entry.header.type !== 'file') {
      entry.resume();
      throw new GridstatorError(
        'BAD_REQUEST',
        `Unexpected entry "${entry.header.name}" in blob archive`,
      );
    }
    entries++;
    bytes += entry.header.size ?? 0;
    if (limits.maxEntries !== undefined && entries > limits.maxEntries) {
      throw new GridstatorError('PAYLOAD_TOO_LARGE', 'Too many blobs in one upload');
    }
    if (limits.maxBytes !== undefined && bytes > limits.maxBytes) {
      throw new GridstatorError('PAYLOAD_TOO_LARGE', 'Blob upload too large');
    }
    const digest = `sha256:${match[1]}`;
    const isNew = await store.put(digest, entry as AsyncIterable<Uint8Array>, entry.header.size);
    if (isNew) stored++;
    else alreadyPresent++;
  }
  return { stored, alreadyPresent, bytes };
}

export interface WorkspaceTarOptions {
  uid?: number;
  gid?: number;
  /** mtime for all entries; defaults to now. */
  mtime?: Date;
}

/**
 * Builds a tar of the snapshot's working tree from blobs in `store` (all must be present).
 * Directory entries are emitted first so ownership is explicit. The manifest must already be
 * validated, which guarantees no path escapes the archive root.
 */
export function workspaceTar(
  manifest: Manifest,
  store: BlobStore,
  options: WorkspaceTarOptions = {},
): Readable {
  const pack = tar.pack();
  const uid = options.uid ?? 0;
  const gid = options.gid ?? 0;
  const mtime = options.mtime ?? new Date();
  const dirs = new Set<string>();
  for (const e of manifest.entries) {
    const parts = e.path.split('/');
    for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join('/'));
  }
  void (async () => {
    try {
      for (const dir of [...dirs].sort()) {
        pack.entry({ name: dir, type: 'directory', mode: 0o755, uid, gid, mtime });
      }
      for (const e of manifest.entries) {
        if (e.type === 'symlink') {
          pack.entry({
            name: e.path,
            type: 'symlink',
            linkname: e.target,
            mode: 0o777,
            uid,
            gid,
            mtime,
          });
          continue;
        }
        await addEntry(
          pack,
          {
            name: e.path,
            type: 'file',
            size: e.size,
            mode: e.executable ? 0o755 : 0o644,
            uid,
            gid,
            mtime,
          },
          store.read(e.digest),
        );
      }
      pack.finalize();
    } catch (err) {
      pack.destroy(err as Error);
    }
  })();
  return Readable.from(pack as AsyncIterable<Buffer>, { objectMode: false });
}

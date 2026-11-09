import { createHash, randomBytes } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, open, readdir, readFile, rename, rm, stat, utimes } from 'node:fs/promises';
import { join } from 'node:path';
import type { Readable } from 'node:stream';
import { GridstatorError } from '@gridstator/shared';
import { DigestSchema } from '@gridstator/protocol';

export function digestOf(data: string | Uint8Array): string {
  return 'sha256:' + createHash('sha256').update(data).digest('hex');
}

export function assertDigest(digest: string): void {
  if (!DigestSchema.safeParse(digest).success) {
    throw new GridstatorError('BAD_REQUEST', `Invalid digest "${digest}"`);
  }
}

export interface BlobInfo {
  digest: string;
  size: number;
  mtimeMs: number;
}

/**
 * Content-addressed blob store on the local filesystem: `<root>/sha256/<ab>/<hex>`.
 * Writes go to a temp file, are verified against the expected digest, and are renamed into place,
 * so a blob that exists is always complete and correct.
 */
export class BlobStore {
  readonly root: string;
  private readonly tmpDir: string;

  constructor(root: string) {
    this.root = root;
    this.tmpDir = join(root, 'tmp');
  }

  async init(): Promise<void> {
    await mkdir(join(this.root, 'sha256'), { recursive: true, mode: 0o700 });
    await rm(this.tmpDir, { recursive: true, force: true });
    await mkdir(this.tmpDir, { recursive: true, mode: 0o700 });
  }

  pathFor(digest: string): string {
    assertDigest(digest);
    const hex = digest.slice('sha256:'.length);
    return join(this.root, 'sha256', hex.slice(0, 2), hex);
  }

  async has(digest: string): Promise<boolean> {
    try {
      await stat(this.pathFor(digest));
      return true;
    } catch {
      return false;
    }
  }

  async missing(digests: readonly string[]): Promise<string[]> {
    const unique = [...new Set(digests)];
    const result: string[] = [];
    const batch = 64;
    for (let i = 0; i < unique.length; i += batch) {
      const slice = unique.slice(i, i + batch);
      const present = await Promise.all(slice.map((d) => this.has(d)));
      slice.forEach((d, idx) => {
        if (!present[idx]) result.push(d);
      });
    }
    return result;
  }

  /**
   * Stores the content under `digest` after verifying it. Returns false if the blob was already
   * present. Throws BAD_REQUEST on digest or size mismatch.
   */
  async put(
    digest: string,
    source: AsyncIterable<Uint8Array>,
    expectedSize?: number,
  ): Promise<boolean> {
    const finalPath = this.pathFor(digest);
    const tmp = join(this.tmpDir, randomBytes(8).toString('hex'));
    const hash = createHash('sha256');
    let size = 0;
    const handle = await open(tmp, 'wx', 0o600);
    try {
      try {
        for await (const chunk of source) {
          hash.update(chunk);
          size += chunk.length;
          await handle.write(chunk);
        }
      } finally {
        await handle.close();
      }
      const actual = 'sha256:' + hash.digest('hex');
      if (actual !== digest) {
        throw new GridstatorError('BAD_REQUEST', `Blob content does not match digest ${digest}`, {
          hint: 'The file probably changed while it was being uploaded. Retry the command.',
        });
      }
      if (expectedSize !== undefined && size !== expectedSize) {
        throw new GridstatorError('BAD_REQUEST', `Blob ${digest} has unexpected size`);
      }
      if (await this.has(digest)) {
        await rm(tmp, { force: true });
        return false;
      }
      await mkdir(join(finalPath, '..'), { recursive: true, mode: 0o700 });
      await rename(tmp, finalPath);
      return true;
    } catch (err) {
      await rm(tmp, { force: true });
      throw err;
    }
  }

  async putBuffer(digest: string, data: Uint8Array): Promise<boolean> {
    return this.put(
      digest,
      (async function* () {
        yield data;
      })(),
      data.length,
    );
  }

  read(digest: string): Readable {
    return createReadStream(this.pathFor(digest));
  }

  async readBuffer(digest: string): Promise<Buffer> {
    try {
      return await readFile(this.pathFor(digest));
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
        throw new GridstatorError('NOT_FOUND', `Blob ${digest} not found`);
      }
      throw err;
    }
  }

  async size(digest: string): Promise<number> {
    return (await stat(this.pathFor(digest))).size;
  }

  /** Marks a blob as recently used (for LRU eviction). */
  async touch(digest: string): Promise<void> {
    const now = new Date();
    await utimes(this.pathFor(digest), now, now).catch(() => undefined);
  }

  async remove(digest: string): Promise<void> {
    await rm(this.pathFor(digest), { force: true });
  }

  async *list(): AsyncGenerator<BlobInfo> {
    const base = join(this.root, 'sha256');
    let prefixes: string[];
    try {
      prefixes = await readdir(base);
    } catch {
      return;
    }
    for (const prefix of prefixes) {
      let names: string[];
      try {
        names = await readdir(join(base, prefix));
      } catch {
        continue;
      }
      for (const name of names) {
        if (!/^[0-9a-f]{64}$/.test(name)) continue;
        try {
          const s = await stat(join(base, prefix, name));
          yield { digest: `sha256:${name}`, size: s.size, mtimeMs: s.mtimeMs };
        } catch {
          // removed concurrently
        }
      }
    }
  }

  /** Evicts least-recently-used blobs until the store is at most `maxBytes`. Returns bytes freed. */
  async evictTo(maxBytes: number, keep: ReadonlySet<string> = new Set()): Promise<number> {
    const blobs: BlobInfo[] = [];
    let total = 0;
    for await (const b of this.list()) {
      blobs.push(b);
      total += b.size;
    }
    if (total <= maxBytes) return 0;
    blobs.sort((a, b) => a.mtimeMs - b.mtimeMs);
    let freed = 0;
    for (const b of blobs) {
      if (total - freed <= maxBytes) break;
      if (keep.has(b.digest)) continue;
      await this.remove(b.digest);
      freed += b.size;
    }
    return freed;
  }
}

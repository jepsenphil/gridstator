import { appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import type { LogEntry } from '@gridstator/protocol';

/**
 * Job logs as NDJSON files (one entry per line). Appends are synchronous so that a log
 * subscription can read the backlog and attach a live listener without interleaving.
 */
export class LogStore {
  private readonly dir: string;
  private readonly maxBytes: number;

  constructor(dir: string, maxBytesPerJob: number) {
    this.dir = dir;
    this.maxBytes = maxBytesPerJob;
    mkdirSync(dir, { recursive: true, mode: 0o700 });
  }

  private path(jobId: string): string {
    if (!/^[0-9A-Z]{26}$/.test(jobId)) throw new Error(`invalid job id ${jobId}`);
    return join(this.dir, `${jobId}.ndjson`);
  }

  /** Appends entries; returns the entries actually written (dropped once the size cap is hit). */
  append(jobId: string, entries: readonly LogEntry[]): LogEntry[] {
    if (entries.length === 0) return [];
    const path = this.path(jobId);
    let size: number;
    try {
      size = statSync(path).size;
    } catch {
      size = 0;
    }
    if (size >= this.maxBytes) return [];
    const text =
      entries.map((e) => JSON.stringify({ s: e.s, t: e.t, o: e.o, d: e.d })).join('\n') + '\n';
    appendFileSync(path, text, { mode: 0o600 });
    return [...entries];
  }

  /** Reads entries with sequence numbers greater than `after`, up to `limit`. */
  read(jobId: string, after: number, limit = Number.POSITIVE_INFINITY): LogEntry[] {
    const path = this.path(jobId);
    if (!existsSync(path)) return [];
    const out: LogEntry[] = [];
    for (const line of readFileSync(path, 'utf8').split('\n')) {
      if (line === '') continue;
      let entry: LogEntry;
      try {
        entry = JSON.parse(line) as LogEntry;
      } catch {
        continue; // torn write after a crash
      }
      if (entry.s > after) {
        out.push(entry);
        if (out.length >= limit) break;
      }
    }
    return out;
  }

  delete(jobId: string): void {
    rmSync(this.path(jobId), { force: true });
  }
}

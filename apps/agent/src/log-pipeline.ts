import { appendFileSync, readFileSync, rmSync } from 'node:fs';
import { StringDecoder } from 'node:string_decoder';
import type { LogEntry, LogStream } from '@gridstator/protocol';

const FLUSH_INTERVAL_MS = 100;
const FLUSH_BYTES = 64 * 1024;
const MAX_PARTIAL = 8 * 1024;
const MASK = '***';

export interface LogPipelineOptions {
  spoolPath: string;
  /** Secret values to mask (values shorter than 4 characters are not masked). */
  secrets: string[];
  maxBytes: number;
  /** Receives every flushed batch, in order. May drop it if the channel is down (the spool keeps it). */
  sink: (entries: LogEntry[]) => void;
}

/**
 * Turns raw process output into sequenced log entries: UTF-8 decoding across chunk boundaries,
 * line buffering, secret masking, size limiting, batching, and an on-disk spool used to replay
 * entries after a reconnect.
 */
export class LogPipeline {
  private seq = 0;
  private bytes = 0;
  private truncated = false;
  private pending: LogEntry[] = [];
  private pendingBytes = 0;
  private timer: NodeJS.Timeout | undefined;
  private readonly decoders = {
    stdout: new StringDecoder('utf8'),
    stderr: new StringDecoder('utf8'),
  };
  private readonly partial = { stdout: '', stderr: '' };
  private readonly secrets: string[];
  private readonly options: LogPipelineOptions;

  constructor(options: LogPipelineOptions) {
    this.options = options;
    this.secrets = [...new Set(options.secrets.filter((s) => s.length >= 4))].sort(
      (a, b) => b.length - a.length,
    );
  }

  get lastSeq(): number {
    return this.seq;
  }

  output(stream: 'stdout' | 'stderr', chunk: Buffer): void {
    const text = this.partial[stream] + this.decoders[stream].write(chunk);
    const lastNewline = text.lastIndexOf('\n');
    if (lastNewline === -1) {
      this.partial[stream] = text;
    } else {
      this.push(stream, text.slice(0, lastNewline + 1));
      this.partial[stream] = text.slice(lastNewline + 1);
    }
    if (this.partial[stream].length > MAX_PARTIAL) {
      this.push(stream, this.partial[stream]);
      this.partial[stream] = '';
    }
    this.schedule();
  }

  system(message: string): void {
    this.push('system', message.endsWith('\n') ? message : `${message}\n`);
    this.schedule();
  }

  private mask(text: string): string {
    let out = text;
    for (const s of this.secrets) if (out.includes(s)) out = out.split(s).join(MASK);
    return out;
  }

  private push(stream: LogStream, text: string): void {
    if (text === '') return;
    if (this.truncated) return;
    const data = this.mask(text);
    if (this.bytes + data.length > this.options.maxBytes) {
      this.truncated = true;
      this.enqueue(
        'system',
        `Output truncated: the job exceeded the ${Math.round(this.options.maxBytes / (1024 * 1024))} MiB log limit\n`,
      );
      return;
    }
    this.bytes += data.length;
    this.enqueue(stream, data);
  }

  private enqueue(stream: LogStream, data: string): void {
    this.pending.push({ s: ++this.seq, t: Date.now(), o: stream, d: data });
    this.pendingBytes += data.length;
    if (this.pendingBytes >= FLUSH_BYTES) this.flush();
  }

  private schedule(): void {
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      for (const s of ['stdout', 'stderr'] as const) {
        if (this.partial[s] !== '') {
          this.push(s, this.partial[s]);
          this.partial[s] = '';
        }
      }
      this.flush();
    }, FLUSH_INTERVAL_MS);
  }

  flush(): void {
    if (this.pending.length === 0) return;
    const entries = this.pending;
    this.pending = [];
    this.pendingBytes = 0;
    appendFileSync(
      this.options.spoolPath,
      entries.map((e) => JSON.stringify(e)).join('\n') + '\n',
      { mode: 0o600 },
    );
    this.options.sink(entries);
  }

  /** Flushes everything, including incomplete lines. */
  finish(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    for (const s of ['stdout', 'stderr'] as const) {
      const rest = this.partial[s] + this.decoders[s].end();
      this.partial[s] = '';
      this.push(s, rest);
    }
    this.flush();
  }
}

/** Reads spooled entries with sequence numbers above `after`. */
export function readSpool(path: string, after: number): LogEntry[] {
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch {
    return [];
  }
  const out: LogEntry[] = [];
  for (const line of text.split('\n')) {
    if (!line) continue;
    try {
      const e = JSON.parse(line) as LogEntry;
      if (e.s > after) out.push(e);
    } catch {
      // torn line after a crash
    }
  }
  return out;
}

export function removeSpool(path: string): void {
  rmSync(path, { force: true });
}

import { StringDecoder } from 'node:string_decoder';
import type { JobStep, JobStepStatus } from '@gridstator/protocol';

export interface ActLine {
  stream: 'stdout' | 'stderr' | 'system';
  text: string;
}

interface ActJsonEntry {
  level?: string;
  msg?: string;
  job?: string;
  jobID?: string;
  stage?: string;
  step?: string;
  stepID?: string[];
  stepid?: string[];
  stepResult?: string;
  jobResult?: string;
  executionTime?: number;
  raw_output?: boolean;
  time?: string;
  error?: string;
}

const STATUS: Record<string, JobStepStatus> = {
  success: 'success',
  failure: 'failure',
  skipped: 'skipped',
  cancelled: 'cancelled',
};

/**
 * Parses `act --json` output. Produces log lines in act's familiar console format
 * (`[Workflow/job] ⭐ Run Main step`, `[Workflow/job]   | output`) and tracks steps and job
 * results for structured progress.
 */
export class ActLogParser {
  private readonly steps = new Map<string, JobStep>();
  private readonly order: string[] = [];
  readonly jobResults = new Map<string, string>();
  private changed = false;

  /** Parses one line of act output. */
  feed(line: string, origin: 'stdout' | 'stderr'): ActLine[] {
    const trimmed = line.trim();
    if (trimmed === '') return [];
    let e: ActJsonEntry;
    try {
      e = JSON.parse(trimmed) as ActJsonEntry;
      if (typeof e !== 'object' || e === null) throw new Error('not an object');
    } catch {
      return [{ stream: origin, text: `${line}\n` }];
    }
    const msg = e.msg ?? '';
    // act's timestamps have one-second resolution; the receive time is more precise for live runs.
    const ts = Date.now();
    if (!e.job) {
      if (e.level === 'debug' || e.level === 'trace') return [];
      // act reports "not located inside a git repository" style errors for non-job context; keep them visible.
      const text = e.error ? `${msg}: ${e.error}` : msg;
      return text ? [{ stream: 'system', text: `act: ${text}\n` }] : [];
    }
    this.track(e, msg, ts);
    if (e.raw_output) {
      const body = msg.endsWith('\n') ? msg.slice(0, -1) : msg;
      return body
        .split('\n')
        .map((l) => ({ stream: 'stdout' as const, text: `[${e.job}]   | ${l}\n` }));
    }
    if (e.level === 'debug' || e.level === 'trace' || msg.trim() === '') return [];
    const text = e.error ? `${msg.trim()}: ${e.error}` : msg.trim();
    return [{ stream: e.level === 'error' ? 'stderr' : 'stdout', text: `[${e.job}] ${text}\n` }];
  }

  private stepKey(e: ActJsonEntry): string | undefined {
    const id = e.stepID ?? e.stepid;
    if (!id || id.length === 0) return undefined;
    return `${e.job}#${e.stage ?? ''}#${id.join('.')}`;
  }

  private track(e: ActJsonEntry, msg: string, ts: number): void {
    if (e.jobResult && e.job) {
      this.jobResults.set(e.job, e.jobResult);
      this.changed = true;
    }
    const key = this.stepKey(e);
    if (!key || !e.step) return;
    const name = e.stage && e.stage !== 'Main' ? `${e.stage} ${e.step}` : e.step;
    let step = this.steps.get(key);
    if (!step) {
      step = {
        id: key,
        group: e.job ?? null,
        name,
        status: 'running',
        startedAt: null,
        durationMs: null,
      };
      this.steps.set(key, step);
      this.order.push(key);
      this.changed = true;
    }
    if (msg.includes('⭐ Run ') && step.startedAt === null) {
      step.startedAt = ts;
      this.changed = true;
    }
    if (e.stepResult) {
      step.status = STATUS[e.stepResult] ?? 'failure';
      step.durationMs =
        typeof e.executionTime === 'number'
          ? Math.round(e.executionTime / 1e6)
          : step.startedAt !== null
            ? ts - step.startedAt
            : null;
      this.changed = true;
    }
  }

  /** Steps in the order they were first seen. */
  getSteps(): JobStep[] {
    return this.order.map((k) => ({ ...this.steps.get(k)! }));
  }

  /** Returns true once after any step or job result changed. */
  takeChanged(): boolean {
    const c = this.changed;
    this.changed = false;
    return c;
  }

  /** Gives steps still marked running a final status. */
  finishRunning(status: JobStepStatus): void {
    for (const s of this.steps.values()) if (s.status === 'running') s.status = status;
  }

  /** After a cancellation act reports the interrupted step as failed; show it as cancelled. */
  markCancelled(since: number): void {
    for (const s of this.steps.values()) {
      const endedAt = s.startedAt === null ? null : s.startedAt + (s.durationMs ?? 0);
      if (
        s.status === 'running' ||
        (s.status === 'failure' && (endedAt === null || endedAt >= since - 1000))
      ) {
        s.status = 'cancelled';
      }
    }
  }

  failedJobs(): string[] {
    return [...this.jobResults].filter(([, r]) => r !== 'success').map(([j]) => j);
  }
}

/** Splits a byte stream into lines, keeping partial lines between chunks. */
export class LineSplitter {
  private rest = '';
  private readonly decoder = new StringDecoder('utf8');
  push(chunk: Buffer, onLine: (line: string) => void): void {
    const text = this.rest + this.decoder.write(chunk);
    const lines = text.split('\n');
    this.rest = lines.pop() ?? '';
    for (const l of lines) onLine(l);
  }
  end(onLine: (line: string) => void): void {
    this.rest += this.decoder.end();
    if (this.rest !== '') onLine(this.rest);
    this.rest = '';
  }
}

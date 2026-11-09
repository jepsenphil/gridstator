import { spawn } from 'node:child_process';
import type { ExecutionContext } from './executor.js';

export interface ProcessOutcome {
  exitCode: number | null;
  /** Why Gridstator stopped the process, if it did. */
  stopReason: 'cancelled' | 'timeout' | undefined;
  /** Set when the process could not be started at all. */
  spawnError: string | undefined;
}

export interface RunProcessOptions {
  bin: string;
  args: string[];
  env: NodeJS.ProcessEnv;
  cwd?: string;
  timeoutSeconds: number | undefined;
  /** Seconds between the interrupt signal and SIGKILL. */
  graceSeconds: number;
  /** Receives stdout; defaults to the job's stdout. */
  onStdout?: (chunk: Buffer) => void;
  onStderr?: (chunk: Buffer) => void;
}

/**
 * Runs a host process for a job (e.g. `docker buildx build`, `act`), streaming its output,
 * interrupting it with SIGINT on cancel/timeout (both buildx and act clean up on SIGINT) and
 * killing it if it does not exit within the grace period.
 */
export function runProcess(
  ctx: ExecutionContext,
  options: RunProcessOptions,
): Promise<ProcessOutcome> {
  return new Promise((resolve) => {
    const child = spawn(options.bin, options.args, {
      env: options.env,
      cwd: options.cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    child.stdout.on('data', options.onStdout ?? ((c: Buffer) => ctx.output('stdout', c)));
    child.stderr.on('data', options.onStderr ?? ((c: Buffer) => ctx.output('stderr', c)));
    let stopReason: ProcessOutcome['stopReason'];
    let killTimer: NodeJS.Timeout | undefined;
    const stop = (reason: 'cancelled' | 'timeout') => {
      if (stopReason) return;
      stopReason = reason;
      ctx.system(
        reason === 'timeout'
          ? `Timed out after ${options.timeoutSeconds}s; stopping`
          : 'Cancelling: stopping the process',
      );
      child.kill('SIGINT');
      killTimer = setTimeout(() => child.kill('SIGKILL'), options.graceSeconds * 1000);
    };
    const onAbort = () => stop('cancelled');
    ctx.signal.addEventListener('abort', onAbort, { once: true });
    const timer = options.timeoutSeconds
      ? setTimeout(() => stop('timeout'), options.timeoutSeconds * 1000)
      : undefined;
    const cleanup = () => {
      ctx.signal.removeEventListener('abort', onAbort);
      if (timer) clearTimeout(timer);
      if (killTimer) clearTimeout(killTimer);
    };
    child.on('error', (err) => {
      cleanup();
      resolve({ exitCode: null, stopReason, spawnError: err.message });
    });
    child.on('spawn', () => ctx.onRunning());
    child.on('close', (code) => {
      cleanup();
      resolve({ exitCode: code, stopReason, spawnError: undefined });
    });
    if (ctx.signal.aborted) stop('cancelled');
  });
}

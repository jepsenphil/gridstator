import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cp, mkdir, readFile } from 'node:fs/promises';
import type { Readable } from 'node:stream';
import {
  JobSpecSchema,
  type Artifact,
  type JobOutputs,
  type JobSpecInput,
} from '@gridstator/protocol';
import type { ExecutionContext } from './executor.js';

export function commandAvailable(bin: string, args: string[]): boolean {
  try {
    execFileSync(bin, args, { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

let counter = 0;

export interface TestContext {
  context: ExecutionContext;
  out: string[];
  err: string[];
  system: string[];
  outputs: JobOutputs[];
  uploaded: Array<Artifact & { content: Buffer }>;
  isRunning: () => boolean;
}

/** An ExecutionContext backed by in-memory collectors; `sourceDir` stands in for the snapshot. */
export async function testContext(
  spec: JobSpecInput,
  opts: { workDir: string; sourceDir?: string; tar?: () => Readable; signal?: AbortSignal },
): Promise<TestContext> {
  counter++;
  const jobId = `01TEST${Date.now().toString(36).toUpperCase()}${String(counter).padStart(4, '0')}`
    .slice(0, 26)
    .padEnd(26, '0');
  const out: string[] = [];
  const err: string[] = [];
  const system: string[] = [];
  const outputs: JobOutputs[] = [];
  const uploaded: TestContext['uploaded'] = [];
  let running = false;
  await mkdir(opts.workDir, { recursive: true });
  const context: ExecutionContext = {
    jobId,
    assignmentId: 'a1',
    nodeId: 'test-node',
    nodeName: 'test-node',
    attempt: 1,
    spec: JobSpecSchema.parse(spec),
    workspaceTar: opts.tar ?? null,
    materialize: opts.sourceDir ? (dest) => cp(opts.sourceDir!, dest, { recursive: true }) : null,
    workDir: opts.workDir,
    output: (s, c) => (s === 'stdout' ? out : err).push(c.toString()),
    system: (m) => system.push(m),
    onRunning: () => (running = true),
    uploadArtifact: async (name, filePath) => {
      const content = await readFile(filePath);
      const a = {
        name,
        digest: 'sha256:' + createHash('sha256').update(content).digest('hex'),
        size: content.length,
      };
      uploaded.push({ ...a, content });
      return a;
    },
    reportOutputs: (o) => outputs.push(o),
    signal: opts.signal ?? new AbortController().signal,
  };
  return { context, out, err, system, outputs, uploaded, isRunning: () => running };
}

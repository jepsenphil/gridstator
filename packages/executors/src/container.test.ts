import { execFileSync } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import tar from 'tar-stream';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { JobSpecSchema, type JobSpecInput } from '@gridstator/protocol';
import {
  ContainerExecutor,
  DockerCli,
  LABEL_JOB,
  containerName,
  workspaceVolumeName,
  type ExecutionContext,
} from './index.js';

const IMAGE = 'alpine:3.20';

function dockerAvailable(): boolean {
  try {
    execFileSync('docker', ['version', '--format', '{{.Server.Version}}'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

const available = dockerAvailable();
let tmp: string;
const docker = new DockerCli();

beforeAll(async () => {
  tmp = await mkdtemp(join(tmpdir(), 'gst-exec-'));
  if (available) execFileSync('docker', ['pull', '-q', IMAGE], { stdio: 'ignore' });
});
afterAll(async () => {
  await rm(tmp, { recursive: true, force: true });
});

let counter = 0;
function ctx(
  spec: Partial<JobSpecInput> & { command: string[] },
  opts: { tar?: () => Readable; signal?: AbortSignal } = {},
) {
  counter++;
  const jobId = `01TEST${Date.now().toString(36).toUpperCase()}${String(counter).padStart(4, '0')}`
    .slice(0, 26)
    .padEnd(26, '0');
  const out: string[] = [];
  const err: string[] = [];
  const system: string[] = [];
  let running = false;
  const context: ExecutionContext = {
    jobId,
    assignmentId: 'a1',
    nodeId: 'test-node',
    nodeName: 'test-node',
    attempt: 1,
    spec: JobSpecSchema.parse({
      kind: 'command',
      image: IMAGE,
      resources: { cpus: 1, memoryMb: 256 },
      ...spec,
    }),
    workspaceTar: opts.tar ?? null,
    output: (s, c) => (s === 'stdout' ? out : err).push(c.toString()),
    system: (m) => system.push(m),
    onRunning: () => (running = true),
    signal: opts.signal ?? new AbortController().signal,
  };
  return { context, out, err, system, isRunning: () => running };
}

describe.skipIf(!available)('ContainerExecutor (real Docker)', () => {
  it('runs a command, streams output, reports exit codes, and cleans up', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp });
    const ok = ctx({ command: ['sh', '-c', 'echo out; echo err >&2; echo "$GRIDSTATOR_JOB_ID"'] });
    const r1 = await exec.run(ok.context);
    expect(r1).toEqual({ outcome: 'succeeded', exitCode: 0, error: null, oomKilled: false });
    expect(ok.out.join('')).toContain('out\n');
    expect(ok.out.join('')).toContain(ok.context.jobId);
    expect(ok.err.join('')).toContain('err\n');
    expect(ok.isRunning()).toBe(true);
    const left = await docker.listContainers([`${LABEL_JOB}=${ok.context.jobId}`]);
    expect(left).toHaveLength(0);
    const vols = await docker.listVolumes([`${LABEL_JOB}=${ok.context.jobId}`]);
    expect(vols).toHaveLength(0);

    const bad = ctx({ command: ['sh', '-c', 'exit 3'] });
    expect(await exec.run(bad.context)).toMatchObject({ outcome: 'failed', exitCode: 3 });
  });

  it('passes env and multi-line secrets without putting them on the command line', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp });
    const c = ctx({
      command: [
        'sh',
        '-c',
        'echo "$PLAIN"; printf "%s" "$MULTI" | wc -l; echo "$npm_config_cache"',
      ],
      env: { PLAIN: 'hello world' },
      secretEnv: { MULTI: 'line1\nline2\nline3' },
    });
    const r = await exec.run(c.context);
    expect(r.outcome).toBe('succeeded');
    const text = c.out.join('');
    expect(text).toContain('hello world');
    expect(text).toMatch(/\b2\b/);
    expect(text).toContain('/gridstator/cache/npm');
  });

  it('copies the workspace tar into /workspace and honours workdir', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp });
    const makeTar = () => {
      const pack = tar.pack();
      pack.entry({ name: 'sub', type: 'directory', mode: 0o755 });
      pack.entry({ name: 'sub/hello.txt', mode: 0o644 }, 'from snapshot\n');
      pack.finalize();
      return Readable.from(pack as AsyncIterable<Buffer>, { objectMode: false });
    };
    const c = ctx(
      { command: ['sh', '-c', 'pwd; cat hello.txt'], workdir: 'sub' },
      { tar: makeTar },
    );
    const r = await exec.run(c.context);
    expect(r.outcome).toBe('succeeded');
    expect(c.out.join('')).toBe('/workspace/sub\nfrom snapshot\n');
  });

  it('cancels a running job and removes its resources', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp, stopGraceSeconds: 2 });
    const ac = new AbortController();
    const c = ctx({ command: ['sh', '-c', 'echo started; sleep 300'] }, { signal: ac.signal });
    const promise = exec.run(c.context);
    const t0 = Date.now();
    while (!c.out.join('').includes('started')) {
      if (Date.now() - t0 > 30_000) throw new Error('job never started');
      await new Promise((r) => setTimeout(r, 100));
    }
    ac.abort();
    const r = await promise;
    expect(r.outcome).toBe('cancelled');
    expect(Date.now() - t0).toBeLessThan(15_000);
    const leftover = await docker.exec(['inspect', containerName(c.context.jobId)]);
    expect(leftover.code).not.toBe(0);
    const vol = await docker.exec(['volume', 'inspect', workspaceVolumeName(c.context.jobId)]);
    expect(vol.code).not.toBe(0);
  });

  it('enforces timeouts', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp, stopGraceSeconds: 1 });
    const c = ctx({ command: ['sh', '-c', 'sleep 60'], timeoutSeconds: 2 });
    const r = await exec.run(c.context);
    expect(r).toMatchObject({ outcome: 'failed', error: 'Timed out after 2s' });
  });

  it('reports a clear error for a missing image', async () => {
    const exec = new ContainerExecutor({ docker, tmpDir: tmp });
    const c = ctx({ command: ['true'], image: 'gridstator-does-not-exist.invalid/nope:1' });
    const r = await exec.run(c.context);
    expect(r.outcome).toBe('failed');
    expect(r.error).toMatch(/Could not create container/);
  });
});

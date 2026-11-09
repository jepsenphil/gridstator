/**
 * End-to-end tests: real controller and agent processes (the built CLI), real TLS, real Docker.
 * Two agents run on this machine with separate data directories and node identities, which
 * exercises the same code paths as two physical machines (see docs/DEVELOPMENT.md for the manual
 * multi-machine procedure).
 *
 * Run with: pnpm test:e2e   (requires Docker; pulls alpine:3.20)
 */
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

const CLI = fileURLToPath(new URL('../../apps/cli/dist/main.js', import.meta.url));
const IMAGE = 'alpine:3.20';
const EXAMPLES = fileURLToPath(new URL('../../examples/', import.meta.url));
const actAvailable = (() => {
  try {
    execFileSync('act', ['--version'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
})();

let root: string;
let port: number;
let env: NodeJS.ProcessEnv;
const procs = new Map<string, { child: ChildProcess; output: string[] }>();

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      srv.close(() =>
        typeof addr === 'object' && addr ? resolve(addr.port) : reject(new Error('no port')),
      );
    });
  });
}

interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

function cli(
  args: string[],
  opts: { cwd?: string; extraEnv?: NodeJS.ProcessEnv; timeoutMs?: number } = {},
): Promise<CliResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [CLI, ...args], {
      cwd: opts.cwd ?? root,
      env: { ...env, ...opts.extraEnv },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d: Buffer) => (stdout += d.toString()));
    child.stderr.on('data', (d: Buffer) => (stderr += d.toString()));
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`gridstator ${args.join(' ')} timed out\n${stdout}\n${stderr}`));
    }, opts.timeoutMs ?? 60_000);
    child.on('close', (code) => {
      clearTimeout(timer);
      resolve({ code: code ?? -1, stdout, stderr });
    });
  });
}

async function cliJson<T>(args: string[], cwd?: string): Promise<T> {
  const res = await cli([...args, '--json'], cwd ? { cwd } : {});
  if (res.code !== 0)
    throw new Error(`gridstator ${args.join(' ')} failed (${res.code}): ${res.stderr}`);
  return JSON.parse(res.stdout) as T;
}

function startProcess(name: string, args: string[]): void {
  const child = spawn(process.execPath, [CLI, ...args], {
    cwd: root,
    env: { ...env, GRIDSTATOR_LOG_FORMAT: 'json' },
  });
  const output: string[] = [];
  child.stdout.on('data', (d: Buffer) => output.push(d.toString()));
  child.stderr.on('data', (d: Buffer) => output.push(d.toString()));
  procs.set(name, { child, output });
}

async function stopProcess(name: string, signal: NodeJS.Signals = 'SIGTERM'): Promise<void> {
  const p = procs.get(name);
  if (!p) return;
  procs.delete(name);
  if (p.child.exitCode !== null) return;
  const exited = new Promise((r) => p.child.once('exit', r));
  p.child.kill(signal);
  await exited;
}

async function waitFor<T>(
  fn: () => Promise<T | undefined | false>,
  what: string,
  timeoutMs = 30_000,
): Promise<T> {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v;
    if (Date.now() - start > timeoutMs) {
      const logs = [...procs.entries()]
        .map(([n, p]) => `--- ${n}\n${p.output.join('').slice(-3000)}`)
        .join('\n');
      throw new Error(`timed out waiting for ${what}\n${logs}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

interface JobJson {
  id: string;
  state: string;
  nodeName: string | null;
  exitCode: number | null;
  attempt: number;
  retryOf: string | null;
  startedAt: number | null;
  finishedAt: number | null;
  error: string | null;
}
interface NodeJson {
  name: string;
  status: string;
}

const job = async (id: string) => (await cliJson<{ job: JobJson }>(['job', id])).job;
const waitState = (id: string, state: string, timeoutMs = 60_000) =>
  waitFor(
    async () => {
      const j = await job(id);
      return j.state === state ? j : undefined;
    },
    `job ${id} → ${state}`,
    timeoutMs,
  );

async function detach(args: string[], cwd?: string): Promise<string> {
  const res = await cli(['run', '--detach', ...args], cwd ? { cwd } : {});
  if (res.code !== 0) throw new Error(`run failed: ${res.stderr}`);
  return res.stdout.trim();
}

function dockerAvailable(): boolean {
  try {
    execFileSync('docker', ['version', '--format', '{{.Server.Version}}'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

const enabled = process.env['GRIDSTATOR_E2E'] === '1' && dockerAvailable();

describe.skipIf(!enabled)('Gridstator end to end', () => {
  beforeAll(async () => {
    execFileSync('docker', ['pull', '-q', IMAGE], { stdio: 'ignore' });
    root = await mkdtemp(join(tmpdir(), 'gst-e2e-'));
    port = await freePort();
    env = {
      ...process.env,
      GRIDSTATOR_CONTROLLER_DATA: join(root, 'controller'),
      GRIDSTATOR_CONFIG: join(root, 'client.json'),
      GRIDSTATOR_CACHE: join(root, 'cache'),
      GRIDSTATOR_AGENT_DATA: join(root, 'unused-agent'),
      NO_COLOR: '1',
    };
    const init = await cli(['controller', 'init', '--host', '127.0.0.1', '--port', String(port)]);
    expect(init.code).toBe(0);
    startProcess('controller', ['controller', 'start']);
    await waitFor(async () => (await cli(['status'])).code === 0, 'controller to start');

    const fingerprint = (
      JSON.parse(await readFile(join(root, 'client.json'), 'utf8')) as { caFingerprint: string }
    ).caFingerprint;
    const joinToken = (await cli(['token', 'create', '--join', '--uses', '2'])).stdout.trim();
    for (const [name, labels] of [
      ['alpha', 'fast'],
      ['beta', 'slow'],
    ] as const) {
      const res = await cli([
        'agent',
        'join',
        `https://127.0.0.1:${port}`,
        '--token',
        joinToken,
        '--fingerprint',
        fingerprint,
        '--name',
        name,
        '--data-dir',
        join(root, name),
        '--labels',
        labels,
      ]);
      expect(res.code, res.stderr).toBe(0);
      startProcess(name, ['agent', 'start', '--data-dir', join(root, name)]);
    }
    await waitFor(async () => {
      const nodes = await cliJson<NodeJson[]>(['nodes']);
      return nodes.filter((n) => n.status === 'online').length === 2;
    }, 'both agents online');
  }, 180_000);

  afterAll(async () => {
    for (const name of [...procs.keys()]) await stopProcess(name);
    // Only this test's nodes' resources could exist; the agents remove them on shutdown.
    if (root) await rm(root, { recursive: true, force: true });
  });

  it('runs a command on a chosen node, streams output and propagates the exit code', async () => {
    const res = await cli([
      'run',
      '--no-snapshot',
      '--image',
      IMAGE,
      '--node',
      'beta',
      '--',
      'sh',
      '-c',
      'echo "hi from $GRIDSTATOR_NODE_NAME"; echo oops >&2; exit 7',
    ]);
    expect(res.code).toBe(7);
    expect(res.stdout).toContain('hi from beta');
    expect(res.stderr).toContain('oops');
    expect(res.stderr).toMatch(/failed on beta/);
  });

  it('runs the exact uncommitted working tree and never sends secrets', async () => {
    const proj = join(root, 'project');
    await mkdir(join(proj, 'src'), { recursive: true });
    await writeFile(join(proj, 'src', 'value.txt'), 'committed\n');
    await writeFile(join(proj, '.gitignore'), 'build/\n');
    execFileSync('git', ['init', '-q'], { cwd: proj });
    execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'add', '-A'], { cwd: proj });
    execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init'], {
      cwd: proj,
    });
    await writeFile(join(proj, 'src', 'value.txt'), 'uncommitted edit\n');
    await writeFile(join(proj, 'src', 'new.txt'), 'untracked file\n');
    await writeFile(join(proj, '.env'), 'TOKEN=supersecret\n');
    await mkdir(join(proj, 'build'));
    await writeFile(join(proj, 'build', 'out.txt'), 'ignored\n');

    const res = await cli(
      [
        'run',
        '--image',
        IMAGE,
        '--',
        'sh',
        '-c',
        'cat value.txt new.txt; test ! -e ../.env && echo "no .env"; test ! -e ../build && echo "no build"; pwd',
      ],
      { cwd: join(proj, 'src') },
    );
    expect(res.code, res.stderr).toBe(0);
    expect(res.stdout).toBe(
      'uncommitted edit\nuntracked file\nno .env\nno build\n/workspace/src\n',
    );
    expect(res.stderr).toMatch(/\.env: excluded/);

    const snap = await cliJson<{ id: string; entries: Array<{ path: string }> }>(
      ['snapshot', '--dry-run'],
      proj,
    );
    expect(snap.entries.map((e) => e.path)).toEqual(['.gitignore', 'src/new.txt', 'src/value.txt']);
    // Same content → same snapshot id → nothing to upload the second time.
    const again = await cli(['snapshot'], { cwd: proj });
    expect(again.stdout.trim()).toBe(snap.id);
    expect(again.stderr).toMatch(/nothing to upload/);
  });

  it('masks secrets passed with --secret-env', async () => {
    const res = await cli(
      [
        'run',
        '--no-snapshot',
        '--image',
        IMAGE,
        '--secret-env',
        'MY_SECRET',
        '--',
        'sh',
        '-c',
        'echo "value=$MY_SECRET"',
      ],
      {
        extraEnv: { MY_SECRET: 'p4ssw0rd-123' },
      },
    );
    expect(res.code).toBe(0);
    expect(res.stdout).toBe('value=***\n');
    const listed = await cliJson<JobJson[]>(['jobs', '--limit', '1']);
    const detail = await cli(['job', listed[0]!.id, '--json']);
    expect(detail.stdout).not.toContain('p4ssw0rd-123');
  });

  it('runs independent jobs concurrently on different nodes', async () => {
    const a = await detach(['--no-snapshot', '--image', IMAGE, '--', 'sleep', '4']);
    const b = await detach(['--no-snapshot', '--image', IMAGE, '--', 'sleep', '4']);
    const [ja, jb] = [await waitState(a, 'succeeded'), await waitState(b, 'succeeded')];
    expect(new Set([ja.nodeName, jb.nodeName])).toEqual(new Set(['alpha', 'beta']));
    // The two executions overlapped in time.
    expect(Math.max(ja.startedAt!, jb.startedAt!)).toBeLessThan(
      Math.min(ja.finishedAt!, jb.finishedAt!),
    );
  });

  it('honours label requirements and explains unschedulable jobs', async () => {
    const fast = await detach(['--no-snapshot', '--image', IMAGE, '-l', 'fast', '--', 'true']);
    expect((await waitState(fast, 'succeeded')).nodeName).toBe('alpha');
    const none = await detach(['--no-snapshot', '--image', IMAGE, '-l', 'gpu', '--', 'true']);
    const queued = await cliJson<{ job: { stateReason: string } }>(['job', none]);
    expect(queued.job.stateReason).toMatch(/label\(s\) gpu/);
    expect((await cli(['cancel', none])).code).toBe(0);
    await waitState(none, 'cancelled');
  });

  it('cancels a running job, removes its container, and retries it', async () => {
    const id = await detach([
      '--no-snapshot',
      '--image',
      IMAGE,
      '--',
      'sh',
      '-c',
      'echo started; sleep 300',
    ]);
    await waitFor(async () => (await cli(['logs', id])).stdout.includes('started'), 'job to start');
    const t0 = Date.now();
    expect((await cli(['cancel', id])).code).toBe(0);
    await waitState(id, 'cancelled', 20_000);
    expect(Date.now() - t0).toBeLessThan(15_000);
    const leftovers = execFileSync('docker', [
      'ps',
      '-a',
      '-q',
      '--filter',
      `label=io.gridstator.job=${id}`,
    ])
      .toString()
      .trim();
    expect(leftovers).toBe('');

    const retry = await cli(['retry', id, '--detach']);
    const retried = retry.stdout.trim();
    const rj = await waitFor(async () => {
      const j = await job(retried);
      return j.state === 'running' ? j : undefined;
    }, 'retry to run');
    expect(rj).toMatchObject({ attempt: 2, retryOf: id });
    await cli(['cancel', retried]);
    await waitState(retried, 'cancelled', 20_000);
  });

  it('keeps a job running across a controller restart without losing log lines', async () => {
    const id = await detach([
      '--no-snapshot',
      '--image',
      IMAGE,
      '--node',
      'alpha',
      '--',
      'sh',
      '-c',
      'for i in $(seq 1 12); do echo "line $i"; sleep 0.5; done',
    ]);
    await waitFor(async () => (await cli(['logs', id])).stdout.includes('line 2'), 'job output');
    await stopProcess('controller');
    await new Promise((r) => setTimeout(r, 2500));
    startProcess('controller', ['controller', 'start']);
    await waitFor(async () => (await cli(['status'])).code === 0, 'controller restart');
    const done = await waitState(id, 'succeeded', 60_000);
    expect(done.nodeName).toBe('alpha');
    const logs = (await cli(['logs', id])).stdout;
    expect(logs).toBe(Array.from({ length: 12 }, (_, i) => `line ${i + 1}\n`).join(''));
    await waitFor(
      async () => (await cliJson<NodeJson[]>(['nodes'])).every((n) => n.status === 'online'),
      'agents reconnected',
    );
  });

  it('marks a job lost when its agent dies, cleans up, and auto-retries', async () => {
    const id = await detach([
      '--no-snapshot',
      '--image',
      IMAGE,
      '--node',
      'beta',
      '--retries',
      '1',
      '--',
      'sh',
      '-c',
      'echo "attempt $GRIDSTATOR_ATTEMPT"; sleep 3',
    ]);
    await waitState(id, 'running');
    await stopProcess('beta', 'SIGKILL');
    startProcess('beta', ['agent', 'start', '--data-dir', join(root, 'beta')]);
    const lost = await waitState(id, 'lost', 30_000);
    expect(lost.error).toMatch(/restarted or lost track/);
    const orphan = execFileSync('docker', [
      'ps',
      '-a',
      '-q',
      '--filter',
      `label=io.gridstator.job=${id}`,
    ])
      .toString()
      .trim();
    expect(orphan).toBe('');
    const retried = await waitFor(async () => {
      const jobs = await cliJson<JobJson[]>(['jobs', '--limit', '5']);
      return jobs.find((j) => j.retryOf === id);
    }, 'automatic retry');
    const final = await waitState(retried.id, 'succeeded', 60_000);
    expect(final.attempt).toBe(2);
    expect((await cli(['logs', retried.id])).stdout).toBe('attempt 2\n');
  });

  it('builds a Docker image remotely from the working tree', async () => {
    const proj = join(root, 'docker-build');
    await cp(join(EXAMPLES, 'docker-build'), proj, { recursive: true });
    const res = await cli(
      [
        'build',
        '--node',
        'beta',
        '--target',
        'runner',
        '-t',
        'gridstator-e2e:latest',
        '--build-arg',
        'APP_VERSION=7.7.7',
      ],
      { cwd: proj, timeoutMs: 300_000 },
    );
    expect(res.code, res.stderr).toBe(0);
    expect(res.stderr).toMatch(/Image sha256:[0-9a-f]+ \(gridstator-e2e:latest\)/);
    const jobs = await cliJson<Array<JobJson & { outputs: { image?: { id: string } } }>>([
      'jobs',
      '--limit',
      '1',
    ]);
    const built = jobs[0]!;
    expect(built.nodeName).toBe('beta');
    const labels = execFileSync('docker', [
      'image',
      'inspect',
      'gridstator-e2e:latest',
      '--format',
      '{{index .Config.Labels "io.gridstator.job"}}',
    ])
      .toString()
      .trim();
    expect(labels).toBe(built.id);
    expect(built.outputs.image?.id).toMatch(/^sha256:/);
    execFileSync('docker', ['image', 'rm', 'gridstator-e2e:latest'], { stdio: 'ignore' });
  });

  it.skipIf(!actAvailable)(
    'runs a GitHub Actions workflow against uncommitted changes and returns artifacts',
    async () => {
      const proj = join(root, 'gha');
      await cp(join(EXAMPLES, 'github-actions'), proj, { recursive: true });
      execFileSync('git', ['init', '-q', '-b', 'main'], { cwd: proj });
      execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'add', '-A'], {
        cwd: proj,
      });
      execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init'], {
        cwd: proj,
      });
      // Uncommitted change that the workflow's test depends on.
      await writeFile(
        join(proj, 'src', 'greet.js'),
        'export function greet(name) {\n  return `Hi, ${name}!`;\n}\n',
      );
      await writeFile(
        join(proj, 'test', 'greet.test.js'),
        "import assert from 'node:assert/strict';\nimport { test } from 'node:test';\nimport { greet } from '../src/greet.js';\ntest('greet', () => assert.equal(greet('x'), 'Hi, x!'));\n",
      );

      const list = await cliJson<Array<{ file: string; jobs: Array<{ id: string }> }>>(
        ['actions', 'list'],
        proj,
      );
      expect(list[0]?.jobs.map((j) => j.id)).toEqual(['test', 'lint']);

      const res = await cli(['actions', 'run', 'ci.yml', '--job', 'test', '--node', 'alpha'], {
        cwd: proj,
        timeoutMs: 300_000,
      });
      expect(res.code, res.stdout + res.stderr).toBe(0);
      expect(res.stdout).toContain('[CI/test]   | ✔ greet');
      const jobs = await cliJson<
        Array<
          JobJson & {
            artifacts: Array<{ name: string }>;
            outputs: { steps?: Array<{ name: string; status: string }> };
          }
        >
      >(['jobs', '--limit', '1']);
      const ran = jobs[0]!;
      expect(ran.outputs.steps?.find((s) => s.name === 'Run tests')?.status).toBe('success');
      expect(ran.artifacts.map((a) => a.name)).toEqual(['test-report/test-report.zip']);
      const out = join(root, 'report.zip');
      expect(
        (await cli(['artifacts', ran.id, '--download', 'test-report/test-report.zip', '-o', out]))
          .code,
      ).toBe(0);
      expect((await readFile(out)).subarray(0, 2).toString()).toBe('PK');
    },
  );

  it('reports a healthy setup in doctor', async () => {
    const res = await cli(['doctor']);
    expect(res.stdout).toMatch(/✓ controller/);
    expect(res.stdout).toMatch(/2\/2 online/);
  });
});

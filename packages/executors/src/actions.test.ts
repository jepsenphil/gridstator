import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { ActionsJobSpecSchema } from '@gridstator/protocol';
import { ActionsExecutor, DockerCli, actArgs, dotenvLine } from './index.js';
import { commandAvailable, testContext } from './test-helpers.js';

const available =
  commandAvailable('act', ['--version']) &&
  commandAvailable('docker', ['version']) &&
  commandAvailable('git', ['--version']);
const IMAGE = 'node:24-bookworm-slim';
const docker = new DockerCli();
let tmp: string;
let src: string;

const WORKFLOW = `name: Test
on:
  push:
  workflow_dispatch:
    inputs:
      who: { description: Who, required: false }
jobs:
  ok:
    runs-on: ubuntu-latest
    steps:
      # act alone would run the real checkout for this (raw ref != github.ref).
      - uses: actions/checkout@v4
        with: { ref: "\${{ github.sha }}", persist-credentials: false }
      - name: Show snapshot file
        run: cat value.txt
      # The job must see its workflow exactly as written, not act's rewritten copy.
      - name: Workflow as written
        run: |
          grep -q '[w]ith: { ref:' .github/workflows/test.yml && echo workflow-as-written
      - name: Context
        run: echo "repo=$GITHUB_REPOSITORY ref=$GITHUB_REF_NAME input=\${{ inputs.who }}"
      - name: Secrets
        run: |
          echo "single=\${{ secrets.SINGLE }}"
          printf '%s' "\${{ secrets.MULTI }}" | wc -l
      - run: mkdir -p out && echo artifact-content > out/a.txt
      - uses: actions/upload-artifact@v4
        with: { name: results, path: out/ }
  fails:
    runs-on: ubuntu-latest
    steps:
      - run: echo about to fail && exit 3
  nested:
    runs-on: ubuntu-latest
    steps:
      - name: Prepare nested container input
        run: mkdir -p exchange && echo from-runner > exchange/input.txt
      - name: Run a container with the workspace mounted
        run: |
          node --input-type=module <<'NODE'
          import assert from 'node:assert/strict';
          import http from 'node:http';
          const request = (method, path, body) => new Promise((resolve, reject) => {
            const req = http.request({
              socketPath: '/var/run/docker.sock', method, path,
              headers: { 'content-type': 'application/json' },
            }, (res) => {
              let data = '';
              res.setEncoding('utf8');
              res.on('data', chunk => data += chunk);
              res.on('end', () => res.statusCode >= 300
                ? reject(new Error(data)) : resolve(data ? JSON.parse(data) : {}));
            });
            req.on('error', reject);
            req.end(body === undefined ? undefined : JSON.stringify(body));
          });
          const child = await request('POST', '/containers/create', {
            Image: 'node:24-bookworm-slim',
            Cmd: ['node', '-e', 'const fs = require("node:fs"); if (fs.readFileSync("/workspace/exchange/input.txt", "utf8").trim() !== "from-runner") process.exit(1); fs.writeFileSync("/workspace/exchange/output.txt", "from-child");'],
            HostConfig: { Binds: [process.cwd() + ':/workspace'] },
          });
          try {
            await request('POST', '/containers/' + child.Id + '/start');
            const result = await request('POST', '/containers/' + child.Id + '/wait');
            assert.equal(result.StatusCode, 0, 'nested container must see files created by the runner');
          } finally {
            await request('DELETE', '/containers/' + child.Id + '?force=true');
          }
          NODE
      - name: Read nested container output
        run: test "$(cat exchange/output.txt)" = from-child && echo nested-volume-roundtrip
  calls:
    uses: ./.github/workflows/called.yml
  slow:
    runs-on: ubuntu-latest
    steps:
      - run: |
          mkdir -p cancellation-created && echo cleanup > cancellation-created/file.txt
          for i in $(seq 1 120); do echo "tick $i"; sleep 1; done
`;

const CALLED = `on: workflow_call
jobs:
  inner:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { ref: "\${{ github.sha }}" }
      - name: Called workflow sees the snapshot
        run: cat value.txt
`;

beforeAll(async () => {
  tmp = await mkdtemp(join(tmpdir(), 'gst-act-'));
  src = join(tmp, 'src');
  await mkdir(join(src, '.github', 'workflows'), { recursive: true });
  await writeFile(join(src, '.github', 'workflows', 'test.yml'), WORKFLOW);
  await writeFile(join(src, '.github', 'workflows', 'called.yml'), CALLED);
  await writeFile(join(src, 'value.txt'), 'from the snapshot\n');
  if (available) execFileSync('docker', ['pull', '-q', IMAGE], { stdio: 'ignore' });
});
afterAll(async () => {
  await rm(tmp, { recursive: true, force: true });
});

function executor() {
  return new ActionsExecutor({
    docker,
    actBin: 'act',
    actHome: join(tmp, 'act-home'),
    tmpDir: tmp,
    stopGraceSeconds: 10,
  });
}

const base = {
  kind: 'actions' as const,
  workflow: '.github/workflows/test.yml',
  platforms: { 'ubuntu-latest': IMAGE },
  git: { branch: 'feature/x', remoteUrl: 'https://github.com/acme/widgets.git' },
};

describe('actArgs', () => {
  it('locks down act defaults and never passes secret values as arguments', () => {
    const spec = ActionsJobSpecSchema.parse({
      ...base,
      job: 'ok',
      inputs: { who: 'me' },
      secretEnv: { S: 'v' },
    });
    const args = actArgs(
      spec,
      {
        src: '/s',
        workflow: '/s/w.yml',
        secretFile: '/t/s',
        varFile: '/t/v',
        envFile: '/t/e',
        artifactDir: '/a',
        artifactPort: 40000,
        cacheDir: '/c',
      },
      { jobId: 'J', nodeId: 'N' },
    ).join(' ');
    expect(args).toContain('--container-daemon-socket -');
    expect(args).toContain('--secret-file /t/s');
    expect(args).toContain('--cache-server-addr 127.0.0.1');
    expect(args).toContain('--label io.gridstator.job=J');
    expect(args).toContain('-j ok');
    expect(args).toContain('--input who=me');
    expect(args).toContain('-P ubuntu-latest=node:24-bookworm-slim');
    expect(args).not.toContain('--no-skip-checkout');
    expect(dotenvLine('K', 'a\nb"c')).toBe('K="a\\nb\\"c"');
  });
});

describe.skipIf(!available)('ActionsExecutor (real act)', { timeout: 180_000 }, () => {
  it('runs a job against the snapshot with secrets, inputs, steps and artifacts', async () => {
    const tc = await testContext(
      {
        ...base,
        job: 'ok',
        event: 'workflow_dispatch',
        inputs: { who: 'tester' },
        secretEnv: { SINGLE: 'single-secret-value', MULTI: 'l1\nl2\nl3' },
      },
      { workDir: join(tmp, 'w1'), sourceDir: src },
    );
    const r = await executor().run(tc.context);
    const out = tc.out.join('');
    expect(r.error).toBeNull();
    expect(r.outcome).toBe('succeeded');
    expect(out).toContain('from the snapshot');
    expect(out).toContain('workflow-as-written');
    expect(out).toContain('repo=acme/widgets ref=feature/x input=tester');
    expect(out).toContain('single=***');
    expect(out).not.toContain('single-secret-value');
    expect(out).toMatch(/\|\s+2\n/);
    expect(r.outputs?.steps?.map((s) => s.name)).toEqual(
      expect.arrayContaining(['actions/checkout@v4', 'Show snapshot file', 'Secrets']),
    );
    expect(r.outputs?.steps?.every((s) => s.status === 'success' || s.status === 'skipped')).toBe(
      true,
    );
    expect(tc.outputs.length).toBeGreaterThan(0);
    expect(r.artifacts?.map((a) => a.name)).toEqual(['results/results.zip']);
    expect(
      execFileSync('docker', [
        'ps',
        '-a',
        '-q',
        '--filter',
        `label=io.gridstator.job=${tc.context.jobId}`,
      ])
        .toString()
        .trim(),
    ).toBe('');
  });

  it('points checkouts in a called local workflow at the snapshot', async () => {
    const tc = await testContext(
      { ...base, job: 'calls' },
      { workDir: join(tmp, 'w5'), sourceDir: src },
    );
    const r = await executor().run(tc.context);
    expect(r.error).toBeNull();
    expect(tc.out.join('')).toContain('from the snapshot');
    expect(tc.system.join('\n')).toContain('.github/workflows/called.yml: job inner');
  });

  it('reports failed workflow jobs', async () => {
    const tc = await testContext(
      { ...base, job: 'fails' },
      { workDir: join(tmp, 'w2'), sourceDir: src },
    );
    const r = await executor().run(tc.context);
    expect(r.outcome).toBe('failed');
    expect(r.error).toBe('Workflow job(s) failed: Test/fails');
    expect(r.outputs?.steps?.some((s) => s.status === 'failure')).toBe(true);
  });

  it('shares runner-created files and nested container output when Docker access is enabled', async () => {
    const tc = await testContext(
      { ...base, job: 'nested', dockerSocket: true },
      { workDir: join(tmp, 'nested-work'), sourceDir: src },
    );
    const result = await executor().run(tc.context);
    expect(result.error, tc.out.join('')).toBeNull();
    expect(result.outcome).toBe('succeeded');
    expect(tc.out.join('')).toContain('nested-volume-roundtrip');
    await expect(rm(tc.context.workDir, { recursive: true })).resolves.toBeUndefined();
  });

  it('rejects a workflow path that is not in the snapshot', async () => {
    const tc = await testContext(
      { ...base, workflow: '.github/workflows/missing.yml' },
      { workDir: join(tmp, 'w3'), sourceDir: src },
    );
    expect((await executor().run(tc.context)).error).toMatch(/does not exist/);
  });

  it('cancels a running workflow and removes its containers', async () => {
    const ac = new AbortController();
    const tc = await testContext(
      { ...base, job: 'slow', dockerSocket: true },
      { workDir: join(tmp, 'w4'), sourceDir: src, signal: ac.signal },
    );
    const p = executor().run(tc.context);
    const t0 = Date.now();
    while (!tc.out.join('').includes('tick 2')) {
      if (Date.now() - t0 > 90_000) throw new Error('workflow never started');
      await new Promise((r) => setTimeout(r, 200));
    }
    const running = execFileSync('docker', [
      'ps',
      '-q',
      '--filter',
      `label=io.gridstator.job=${tc.context.jobId}`,
    ])
      .toString()
      .trim();
    expect(running).not.toBe('');
    ac.abort();
    const r = await p;
    expect(r.outcome).toBe('cancelled');
    expect(r.outputs?.steps?.some((s) => s.status === 'cancelled')).toBe(true);
    expect(
      execFileSync('docker', [
        'ps',
        '-a',
        '-q',
        '--filter',
        `label=io.gridstator.job=${tc.context.jobId}`,
      ])
        .toString()
        .trim(),
    ).toBe('');
    await expect(rm(tc.context.workDir, { recursive: true })).resolves.toBeUndefined();
  });
});

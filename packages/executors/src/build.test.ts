import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { DockerBuildJobSpecSchema } from '@gridstator/protocol';
import { DockerBuildExecutor, DockerCli, buildxArgs, exportTag } from './index.js';
import { commandAvailable, testContext } from './test-helpers.js';

const EXAMPLE = fileURLToPath(new URL('../../../examples/docker-build', import.meta.url));
const available =
  commandAvailable('docker', ['buildx', 'version']) && commandAvailable('docker', ['version']);
const docker = new DockerCli();
let tmp: string;

beforeAll(async () => {
  tmp = await mkdtemp(join(tmpdir(), 'gst-build-'));
  if (available)
    for (const img of ['alpine:3.20', 'node:24-bookworm-slim'])
      execFileSync('docker', ['pull', '-q', img], { stdio: 'ignore' });
});
afterAll(async () => {
  await rm(tmp, { recursive: true, force: true });
});

async function fixture(files: Record<string, string>): Promise<string> {
  const dir = await mkdtemp(join(tmp, 'src-'));
  for (const [p, c] of Object.entries(files)) {
    await mkdir(join(dir, p, '..'), { recursive: true });
    await writeFile(join(dir, p), c);
  }
  return dir;
}

describe('buildxArgs', () => {
  it('maps the spec to buildx flags without putting secret values on the command line', () => {
    const spec = DockerBuildJobSpecSchema.parse({
      kind: 'docker-build',
      stage: 'runner',
      buildArgs: { VERSION: '1.0' },
      secretEnv: { NPM_TOKEN: 'secret-value' },
      tags: ['app:dev'],
      platforms: ['linux/amd64'],
      noCache: true,
      output: 'none',
    });
    const args = buildxArgs(
      spec,
      { dockerfile: '/s/Dockerfile', context: '/s', metadataFile: '/m.json', tarFile: '/t.tar' },
      { jobId: 'J', nodeId: 'N' },
    );
    expect(args.join(' ')).toContain('--target runner');
    expect(args.join(' ')).toContain('--build-arg VERSION=1.0');
    expect(args.join(' ')).toContain('--secret id=NPM_TOKEN,env=NPM_TOKEN');
    expect(args.join(' ')).toContain('--label io.gridstator.job=J');
    expect(args.join(' ')).toContain('--output type=cacheonly');
    expect(args.join(' ')).not.toContain('secret-value');
    expect(args[args.length - 1]).toBe('/s');
  });
});

describe.skipIf(!available)('DockerBuildExecutor (real BuildKit)', { timeout: 180_000 }, () => {
  it('builds a stage with build args and secrets, labels and describes the image', async () => {
    const tc = await testContext(
      {
        kind: 'docker-build',
        stage: 'runner',
        tags: ['gridstator-test-build:latest'],
        buildArgs: { APP_VERSION: '9.9.9' },
        secretEnv: { NPM_TOKEN: 'tok-abcdef' },
      },
      { workDir: join(tmp, 'w1'), sourceDir: EXAMPLE },
    );
    const r = await new DockerBuildExecutor({ docker }).run(tc.context);
    expect(r.error).toBeNull();
    expect(r.outcome).toBe('succeeded');
    expect(r.outputs?.image).toMatchObject({
      tags: ['gridstator-test-build:latest'],
      output: 'load',
    });
    expect(r.outputs?.image?.id).toMatch(/^sha256:/);
    expect(r.outputs?.image?.sizeBytes).toBeGreaterThan(0);
    const all = tc.out.join('') + tc.err.join('');
    expect(all).toContain('build secret present');
    expect(all).not.toContain('tok-abcdef');
    const labels = JSON.parse(
      execFileSync('docker', [
        'image',
        'inspect',
        'gridstator-test-build:latest',
        '--format',
        '{{json .Config.Labels}}',
      ]).toString(),
    ) as Record<string, string>;
    expect(labels['io.gridstator.job']).toBe(tc.context.jobId);
    const version = execFileSync('docker', [
      'run',
      '--rm',
      '--entrypoint',
      'cat',
      'gridstator-test-build:latest',
      'dist/server.js',
    ]).toString();
    expect(version).toContain("'9.9.9'");
    execFileSync('docker', ['image', 'rm', 'gridstator-test-build:latest'], { stdio: 'ignore' });
  });

  it('exports an image as a tar artifact and removes the temporary tag', async () => {
    const src = await fixture({ Dockerfile: 'FROM alpine:3.20\nRUN echo hi > /hi\n' });
    const tc = await testContext(
      { kind: 'docker-build', output: 'tar' },
      { workDir: join(tmp, 'w2'), sourceDir: src },
    );
    const r = await new DockerBuildExecutor({ docker }).run(tc.context);
    expect(r.outcome).toBe('succeeded');
    expect(r.artifacts?.map((a) => a.name)).toEqual(['image.tar']);
    expect(tc.uploaded[0]!.content.length).toBeGreaterThan(1000);
    const tags = execFileSync('docker', [
      'images',
      '--format',
      '{{.Repository}}:{{.Tag}}',
    ]).toString();
    expect(tags).not.toContain(exportTag(tc.context.jobId).split(':')[0]);
  });

  it('reports build failures, missing Dockerfiles and paths escaping the snapshot', async () => {
    const exec = new DockerBuildExecutor({ docker });
    const failing = await fixture({ Dockerfile: 'FROM alpine:3.20\nRUN echo boom && false\n' });
    let tc = await testContext(
      { kind: 'docker-build', output: 'none' },
      { workDir: join(tmp, 'w3'), sourceDir: failing },
    );
    expect(await exec.run(tc.context)).toMatchObject({ outcome: 'failed', exitCode: 1 });

    tc = await testContext(
      { kind: 'docker-build', dockerfile: 'nope/Dockerfile' },
      { workDir: join(tmp, 'w4'), sourceDir: failing },
    );
    expect((await exec.run(tc.context)).error).toMatch(
      /Dockerfile "nope\/Dockerfile" does not exist/,
    );

    const escaping = await fixture({ Dockerfile: 'FROM alpine:3.20\n' });
    await symlink('/etc', join(escaping, 'ctx'));
    tc = await testContext(
      { kind: 'docker-build', context: 'ctx' },
      { workDir: join(tmp, 'w5'), sourceDir: escaping },
    );
    expect((await exec.run(tc.context)).error).toMatch(/resolves outside the snapshot/);
  });

  it('cancels a running build', async () => {
    const slow = await fixture({
      Dockerfile: `FROM alpine:3.20\nRUN echo started-${Date.now()} && sleep 120\n`,
    });
    const ac = new AbortController();
    const tc = await testContext(
      { kind: 'docker-build', output: 'none', noCache: true },
      { workDir: join(tmp, 'w6'), sourceDir: slow, signal: ac.signal },
    );
    const p = new DockerBuildExecutor({ docker, stopGraceSeconds: 5 }).run(tc.context);
    const t0 = Date.now();
    while (!(tc.out.join('') + tc.err.join('')).includes('started-')) {
      if (Date.now() - t0 > 60_000) throw new Error('build never started');
      await new Promise((r) => setTimeout(r, 200));
    }
    ac.abort();
    const r = await p;
    expect(r.outcome).toBe('cancelled');
    expect(Date.now() - t0).toBeLessThan(30_000);
  });
});

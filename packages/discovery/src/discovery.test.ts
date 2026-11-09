import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  discoverDockerfiles,
  discoverPackageScripts,
  discoverWorkflows,
  findWorkflow,
  parseDockerfile,
  parseWorkflow,
  scriptCommand,
} from './index.js';

let dir: string;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-disc-'));
});
afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

async function write(rel: string, content: string) {
  await mkdir(join(dir, rel, '..'), { recursive: true });
  await writeFile(join(dir, rel), content);
}

describe('workflows', () => {
  it('lists events, inputs, jobs and default event', () => {
    const w = parseWorkflow(
      '.github/workflows/ci.yml',
      `name: CI
on:
  pull_request:
  workflow_dispatch:
    inputs:
      env: { description: Target, required: true, type: choice, options: [dev, prod] }
      dry: { default: false }
jobs:
  test:
    runs-on: ubuntu-latest
    strategy: { matrix: { node: [20, 22] } }
    steps: [{ uses: actions/checkout@v4 }, { run: npm test }]
  deploy:
    needs: [test]
    runs-on: ubuntu-latest
    environment: production
    steps: [{ run: ./deploy.sh }]
`,
    );
    expect(w.name).toBe('CI');
    expect(w.events).toEqual(['pull_request', 'workflow_dispatch']);
    expect(w.defaultEvent).toBe('workflow_dispatch');
    expect(w.dispatchInputs).toEqual([
      {
        name: 'env',
        description: 'Target',
        required: true,
        type: 'choice',
        options: ['dev', 'prod'],
      },
      { name: 'dry', required: false, default: 'false' },
    ]);
    expect(w.jobs.map((j) => [j.id, j.needs, j.matrix, j.runnable])).toEqual([
      ['test', [], true, true],
      ['deploy', ['test'], false, true],
    ]);
    expect(w.jobs[1]?.warnings.join()).toMatch(/deployment environments/);
  });

  it('flags features act cannot support', () => {
    const w = parseWorkflow(
      'x.yml',
      `on: push
permissions: { id-token: write }
jobs:
  win: { runs-on: windows-latest, steps: [{ run: dir }] }
  remote: { uses: org/repo/.github/workflows/build.yml@main }
  local: { uses: ./.github/workflows/build.yml }
  other:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { repository: org/tools }
      - run: echo "\${{ secrets.GITHUB_TOKEN }}"
  docker: { runs-on: ubuntu-latest, steps: [{ uses: docker/build-push-action@v6 }] }
`,
    );
    expect(w.parseError).toBeUndefined();
    const job = (id: string) => w.jobs.find((j) => j.id === id)!;
    expect(job('win').runnable).toBe(false);
    expect(job('remote').warnings.join()).toMatch(/remote reusable workflow/);
    expect(job('local').warnings).toEqual([]);
    expect(job('other').warnings.join()).toMatch(/clones that repository/);
    expect(job('docker').warnings.join()).toMatch(/Docker socket/);
    expect(w.warnings.join()).toMatch(/GITHUB_TOKEN/);
    expect(w.warnings.join()).toMatch(/OIDC/);
  });

  it('reports parse errors and discovers files', async () => {
    await write(
      '.github/workflows/a.yml',
      'on: push\njobs: { t: { runs-on: ubuntu-latest, steps: [] } }\n',
    );
    await write('.github/workflows/bad.yaml', 'on: [push\n');
    await write('.github/workflows/readme.md', '# not a workflow');
    const found = await discoverWorkflows(dir);
    expect(found.map((w) => w.file)).toEqual([
      '.github/workflows/a.yml',
      '.github/workflows/bad.yaml',
    ]);
    expect(found[1]?.parseError).toBeTruthy();
    expect(findWorkflow(found, 'a.yml')?.file).toBe('.github/workflows/a.yml');
    expect(findWorkflow(found, './.github/workflows/a.yml')?.file).toBe('.github/workflows/a.yml');
  });
});

describe('package scripts and Dockerfiles', () => {
  it('finds scripts per package with the right package manager', async () => {
    await write(
      'package.json',
      JSON.stringify({ name: 'root', scripts: { test: 'vitest' }, packageManager: 'pnpm@10.0.0' }),
    );
    await write(
      'packages/web/package.json',
      JSON.stringify({ name: 'web', scripts: { build: 'next build' } }),
    );
    await write('node_modules/x/package.json', JSON.stringify({ name: 'x', scripts: { a: 'b' } }));
    const found = await discoverPackageScripts(dir);
    expect(found.map((p) => [p.dir, p.name, p.packageManager, Object.keys(p.scripts)])).toEqual([
      ['.', 'root', 'pnpm', ['test']],
      ['packages/web', 'web', 'pnpm', ['build']],
    ]);
    expect(scriptCommand('pnpm', 'build', 'packages/web')).toBe(
      'corepack enable && pnpm install --frozen-lockfile && cd "packages/web" && pnpm run "build"',
    );
    expect(scriptCommand('npm', 'test', '.')).toBe('npm ci && npm run "test"');
  });

  it('finds Dockerfiles and their stages', async () => {
    await write(
      'Dockerfile',
      'FROM node:24 AS deps\nFROM deps as build\nFROM --platform=$BUILDPLATFORM nginx:alpine AS runner\n',
    );
    await write('services/api/api.Dockerfile', 'FROM python:3.13\n');
    expect(parseDockerfile('FROM a AS b\n# FROM c AS d\n').stages).toEqual(['b']);
    const found = await discoverDockerfiles(dir);
    expect(found).toEqual([
      {
        file: 'Dockerfile',
        context: '.',
        stages: ['deps', 'build', 'runner'],
        baseImages: ['node:24', 'deps', 'nginx:alpine'],
      },
      {
        file: 'services/api/api.Dockerfile',
        context: 'services/api',
        stages: [],
        baseImages: ['python:3.13'],
      },
    ]);
  });
});

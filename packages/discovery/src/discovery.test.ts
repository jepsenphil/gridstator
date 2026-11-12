import { describe, expect, it } from 'vitest';
import { parseWorkflow, scriptCommand } from './index.js';

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
});

describe('script commands', () => {
  it('uses the selected package manager and package directory', () => {
    expect(scriptCommand('pnpm', 'build', 'packages/web')).toBe(
      'corepack enable && pnpm install --frozen-lockfile && cd "packages/web" && pnpm run "build"',
    );
    expect(scriptCommand('npm', 'test', '.')).toBe('npm ci && npm run "test"');
  });
});

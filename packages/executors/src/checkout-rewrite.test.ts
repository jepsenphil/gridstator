import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import {
  ACT_WORKFLOWS_DIR,
  repoSlug,
  rewriteLocalCheckouts,
  writeActWorkflows,
} from './checkout-rewrite.js';

const F = '.github/workflows/w.yml';

describe('repoSlug', () => {
  it('parses https, ssh and scp remotes', () => {
    expect(repoSlug('https://github.com/acme/widgets.git')).toBe('acme/widgets');
    expect(repoSlug('https://github.com/acme/widgets')).toBe('acme/widgets');
    expect(repoSlug('git@github.com:acme/widgets.git')).toBe('acme/widgets');
    expect(repoSlug('ssh://git@github.com/acme/widgets.git')).toBe('acme/widgets');
    expect(repoSlug(undefined)).toBeUndefined();
  });
});

describe('rewriteLocalCheckouts', () => {
  const wf = `# keep this comment
name: "✅ Checks: Coverage"
on: { workflow_call: {} }
jobs:
  coverage:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          ref: \${{ inputs.source_ref || github.sha }}
          persist-credentials: false
      - uses: actions/checkout@v4
        with:
          ref: main
      - name: Same repo by name
        uses: actions/checkout@v4
        with: { repository: Acme/Widgets, ref: v1, path: sub }
      - name: Same repo by expression
        uses: actions/checkout@v4
        with:
          repository: \${{ github.repository }}
      - name: Other repo
        uses: actions/checkout@v4
        with: { repository: acme/tools, ref: v2 }
      - name: Plain
        uses: actions/checkout@v4
      - name: Not checkout
        uses: actions/setup-node@v4
        with: { ref: x }
`;

  it('removes ref/repository from same-repository checkouts only', () => {
    const { text, rewrites } = rewriteLocalCheckouts(wf, F, 'acme/widgets');
    const steps = (parse(text) as { jobs: { coverage: { steps: { with?: unknown }[] } } }).jobs
      .coverage.steps;
    expect(steps.map((s) => s.with)).toEqual([
      { 'persist-credentials': false },
      undefined,
      { path: 'sub' },
      undefined,
      { repository: 'acme/tools', ref: 'v2' },
      undefined,
      { ref: 'x' },
    ]);
    expect(text).toContain('# keep this comment');
    expect(rewrites.map((r) => r.step)).toEqual([
      'Checkout',
      '#2 (actions/checkout@v4)',
      'Same repo by name',
      'Same repo by expression',
    ]);
    expect(rewrites[0]).toEqual({
      file: F,
      job: 'coverage',
      step: 'Checkout',
      removed: { ref: '${{ inputs.source_ref || github.sha }}' },
    });
  });

  it('treats a named repository as foreign when the remote is unknown', () => {
    const { rewrites } = rewriteLocalCheckouts(wf, F);
    expect(rewrites.map((r) => r.step)).not.toContain('Same repo by name');
  });

  it('returns the original text when nothing changes or the YAML is invalid', () => {
    const plain = 'jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n';
    expect(rewriteLocalCheckouts(plain, F)).toEqual({ text: plain, rewrites: [] });
    expect(rewriteLocalCheckouts('jobs: [', F)).toEqual({ text: 'jobs: [', rewrites: [] });
  });
});

describe('writeActWorkflows', () => {
  let root: string;
  const wfDir = () => join(root, '.github', 'workflows');
  const copy = async (name: string) => readFile(join(root, ACT_WORKFLOWS_DIR, name), 'utf8');

  beforeEach(async () => {
    root = await mkdtemp(join(tmpdir(), 'gst-wf-'));
    await mkdir(wfDir(), { recursive: true });
  });
  afterEach(async () => {
    await rm(root, { recursive: true, force: true });
  });

  const caller = `name: Checks
on: push
jobs:
  coverage:
    uses: ./.github/workflows/coverage.yml
  lint:
    uses: "./.github/workflows/lint.yml"
  shared:
    uses: acme/shared/.github/workflows/coverage.yml@v1
`;
  const coverage = `on: workflow_call
jobs:
  run:
    steps:
      - uses: actions/checkout@v4
        with:
          ref: \${{ inputs.source_ref || github.sha }}
          persist-credentials: false
`;
  const lint = 'on: workflow_call\njobs:\n  lint:\n    steps:\n      - uses: actions/checkout@v4\n';

  it('rewrites copies for act and leaves the workflows the job sees as written', async () => {
    await writeFile(join(wfDir(), 'checks.yml'), caller);
    await writeFile(join(wfDir(), 'coverage.yml'), coverage);
    await writeFile(join(wfDir(), 'lint.yml'), lint);

    const { rewrites, copies } = await writeActWorkflows(root, 'acme/widgets');

    expect(rewrites.map((r) => `${r.file}:${r.job}`)).toEqual([
      '.github/workflows/coverage.yml:run',
    ]);
    expect(copies).toEqual(
      new Map(
        ['checks.yml', 'coverage.yml', 'lint.yml'].map((n) => [
          `.github/workflows/${n}`,
          `${ACT_WORKFLOWS_DIR}/${n}`,
        ]),
      ),
    );
    expect(await readFile(join(wfDir(), 'checks.yml'), 'utf8')).toBe(caller);
    expect(await readFile(join(wfDir(), 'coverage.yml'), 'utf8')).toBe(coverage);
    expect(await readFile(join(wfDir(), 'lint.yml'), 'utf8')).toBe(lint);

    expect(parse(await copy('checks.yml'))).toMatchObject({
      jobs: {
        coverage: { uses: `./${ACT_WORKFLOWS_DIR}/coverage.yml` },
        lint: { uses: `./${ACT_WORKFLOWS_DIR}/lint.yml` },
        shared: { uses: 'acme/shared/.github/workflows/coverage.yml@v1' },
      },
    });
    const steps = (parse(await copy('coverage.yml')) as { jobs: { run: { steps: unknown[] } } })
      .jobs.run.steps;
    expect(steps).toEqual([
      { uses: 'actions/checkout@v4', with: { 'persist-credentials': false } },
    ]);
    expect(await copy('lint.yml')).toBe(lint);
  });

  it('writes nothing when no checkout needs rewriting', async () => {
    await writeFile(join(wfDir(), 'lint.yml'), lint);
    expect(await writeActWorkflows(root)).toEqual({ rewrites: [], copies: new Map() });
    expect(existsSync(join(root, '.git'))).toBe(false);
  });

  it('copies invalid workflows verbatim and tolerates a missing workflows directory', async () => {
    await writeFile(join(wfDir(), 'coverage.yml'), coverage);
    await writeFile(join(wfDir(), 'broken.yml'), 'jobs: [');
    await writeActWorkflows(root);
    expect(await copy('broken.yml')).toBe('jobs: [');
    await rm(wfDir(), { recursive: true });
    expect(await writeActWorkflows(root)).toEqual({ rewrites: [], copies: new Map() });
  });
});

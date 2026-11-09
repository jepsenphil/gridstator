import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { repoSlug, rewriteLocalCheckouts } from './checkout-rewrite.js';

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

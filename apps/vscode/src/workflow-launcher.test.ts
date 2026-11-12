import { describe, expect, it } from 'vitest';
import { ProjectConfigSchema } from '@gridstator/protocol';
import { parseWorkflow } from '@gridstator/discovery';
import type { LaunchItem } from './messages';
import { buildSpec } from './launcher';
import { launcherWorkflows } from './workflow-launcher';

const config = {
  ...ProjectConfigSchema.parse({}),
  actions: { launcher: { 'coverage.yml': { jobs: ['coverage'] } } },
};

function item(file = 'coverage.yml'): LaunchItem {
  return {
    id: `workflow:project:.github/workflows/${file}`,
    kind: 'workflow',
    label: file,
    folder: 'project',
    warnings: [],
    workflow: {
      file: `.github/workflows/${file}`,
      events: ['workflow_call'],
      defaultEvent: 'workflow_call',
      inputs: [],
      jobs: ['coverage', 'publish_badge'].map((id) => ({
        id,
        name: id,
        needs: [],
        runnable: true,
        warnings: [],
      })),
    },
  };
}

const project = { config, root: '/project', workdir: '.' };

describe('curated workflow launch boundary', () => {
  it('rejects an excluded workflow even when a stale item is supplied', () => {
    expect(() => buildSpec(item('ci-cd-staging.yml'), {}, project, 'snapshot')).toThrow(
      /not available in the local launcher/,
    );
  });

  it('rejects a hidden job even when a stale item still includes it', () => {
    expect(() => buildSpec(item(), { job: 'publish_badge' }, project, 'snapshot')).toThrow(
      /not available in the local launcher/,
    );
  });

  it('rejects whole-workflow launches that would execute hidden jobs', () => {
    expect(() => buildSpec(item(), {}, project, 'snapshot')).toThrow(
      /Choose one of the available jobs/,
    );
  });

  it('preserves the explicitly selected standalone check', () => {
    expect(buildSpec(item(), { job: 'coverage' }, project, 'snapshot')).toMatchObject({
      kind: 'actions',
      workflow: '.github/workflows/coverage.yml',
      job: 'coverage',
    });
  });
});

describe('curated workflow discovery', () => {
  const workflows = [
    parseWorkflow(
      '.github/workflows/ci-cd-staging.yml',
      'name: Internal staging\non: workflow_call\njobs: {}',
    ),
    parseWorkflow(
      '.github/workflows/coverage.yml',
      `
name: Coverage
on: workflow_call
jobs:
  coverage:
    runs-on: ubuntu-latest
    needs: install
    steps: [{run: npm test}]
  publish_badge:
    runs-on: ubuntu-latest
    steps: [{run: publish}]
`,
    ),
  ];

  it('preserves unconfigured discovery and permits an explicitly empty picker', () => {
    expect(launcherWorkflows(workflows, {}).map((entry) => entry.workflow)).toEqual(workflows);
    expect(launcherWorkflows(workflows, { launcher: {} })).toEqual([]);
  });

  it('shows only configured workflows and jobs, retaining job dependencies', () => {
    const result = launcherWorkflows(workflows, {
      launcher: { 'coverage.yml': { jobs: ['coverage'], description: 'Run coverage locally' } },
    });
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({
      description: 'Run coverage locally',
      wholeWorkflow: false,
      workflow: {
        file: '.github/workflows/coverage.yml',
        jobs: [{ id: 'coverage', needs: ['install'] }],
      },
    });
    expect(workflows[1]!.jobs).toHaveLength(2);
  });

  it('supports relative paths and explicit whole-workflow entry points in configuration order', () => {
    const result = launcherWorkflows(workflows, {
      launcher: { '.github/workflows/coverage.yml': {}, 'ci-cd-staging.yml': {} },
    });
    expect(result.map((entry) => entry.workflow.file)).toEqual([
      '.github/workflows/coverage.yml',
      '.github/workflows/ci-cd-staging.yml',
    ]);
    expect(result[0]!.wholeWorkflow).toBe(true);
  });

  it('reports missing workflow and job names instead of exposing uncurated choices', () => {
    expect(() => launcherWorkflows(workflows, { launcher: { 'missing.yml': {} } })).toThrow(
      /workflow missing.yml does not exist/,
    );
    expect(() =>
      launcherWorkflows(workflows, { launcher: { 'coverage.yml': { jobs: ['missing'] } } }),
    ).toThrow(/job coverage.yml:missing does not exist/);
  });
});

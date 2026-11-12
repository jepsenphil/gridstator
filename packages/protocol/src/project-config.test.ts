import { describe, expect, it } from 'vitest';
import { ProjectConfigSchema } from './project-config.js';

describe('workflow launcher configuration', () => {
  it('keeps omitted, empty, and curated launchers distinct', () => {
    expect(ProjectConfigSchema.parse({}).actions).not.toHaveProperty('launcher');
    expect(ProjectConfigSchema.parse({ actions: { launcher: {} } }).actions.launcher).toEqual({});
    const launcher = {
      'staging-local.yml': { description: 'Build locally', jobs: ['local'] },
      '.github/workflows/coverage.yaml': { jobs: ['coverage', 'report'] },
      'all.yml': {},
    };
    expect(ProjectConfigSchema.parse({ actions: { launcher } }).actions.launcher).toEqual(launcher);
  });

  it.each([
    null,
    [],
    { '../internal.yml': {} },
    { '.github/workflows/../internal.yml': {} },
    { '.github/workflows/nested/internal.yml': {} },
    { 'sub/internal.yml': {} },
    { 'internal\\hidden.yml': {} },
    { 'internal.txt': {} },
    { '.yml': {} },
    { 'internal.yml': null },
    { 'internal.yml': { unknown: true } },
    { 'internal.yml': { description: 1 } },
    { 'internal.yml': { jobs: [] } },
    { 'internal.yml': { jobs: [''] } },
    { 'internal.yml': { jobs: ['  '] } },
    { 'internal.yml': { jobs: [1] } },
    { 'internal.yml': { jobs: ['build', 'build'] } },
    { 'internal.yml': {}, '.github/workflows/internal.yml': {} },
  ])('rejects invalid or ambiguous launcher configuration: %j', (launcher) => {
    expect(ProjectConfigSchema.safeParse({ actions: { launcher } }).success).toBe(false);
  });
});

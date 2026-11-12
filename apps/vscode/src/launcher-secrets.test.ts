import { describe, expect, it } from 'vitest';
import { ProjectConfigSchema } from '@gridstator/protocol';
import type { LaunchItem } from './messages';
import { buildSpec, configuredSecretNames } from './launcher';

const config = ProjectConfigSchema.parse({
  version: 1,
  tasks: { test: { command: 'npm test', secretEnv: ['TOKEN'] } },
  actions: { secretEnv: ['TOKEN'] },
  builds: { web: { secretEnv: ['TOKEN'] } },
});
const project = { root: '/projects/app', workdir: '.', config };
const common = { folder: 'app', warnings: [], label: 'test' };
const items: LaunchItem[] = [
  { ...common, id: 'task', kind: 'task', task: { name: 'test', command: 'npm test' } },
  { ...common, id: 'build', kind: 'build', build: { name: 'web', tags: [], output: 'load' } },
  {
    ...common,
    id: 'workflow',
    kind: 'workflow',
    workflow: {
      file: '.github/workflows/test.yml',
      events: ['workflow_dispatch'],
      defaultEvent: 'workflow_dispatch',
      inputs: [],
      jobs: [],
    },
  },
];

describe('configured job secrets', () => {
  it.each(items)('forwards resolved secrets only in secretEnv for $kind jobs', (item) => {
    const spec = buildSpec(item, { env: { PUBLIC: 'value' } }, project, 'snapshot', undefined, {
      TOKEN: 'private-value',
      UNRELATED: 'never-forward',
    });
    expect(configuredSecretNames(item, config)).toEqual(['TOKEN']);
    expect(spec.secretEnv).toEqual({ TOKEN: 'private-value' });
    expect(spec.env).toEqual({ PUBLIC: 'value' });
    expect(JSON.stringify(spec)).not.toContain('never-forward');
  });

  it.each(items)('rejects plaintext duplicates for $kind before submission', (item) => {
    expect(() =>
      buildSpec(item, { env: { TOKEN: 'never-display' } }, project, 'snapshot', undefined, {
        TOKEN: 'saved',
      }),
    ).toThrow('Secret TOKEN must not be in the plain environment');
  });

  it('also rejects a configured secret placed in task env', () => {
    const changed = ProjectConfigSchema.parse({
      ...config,
      tasks: { test: { ...config.tasks.test, env: { TOKEN: 'never-display' } } },
    });
    expect(() =>
      buildSpec(items[0]!, {}, { ...project, config: changed }, 'snapshot', undefined, {
        TOKEN: 'saved',
      }),
    ).toThrow('Secret TOKEN must not be in the plain environment');
  });

  it('fails closed if resolution was skipped', () => {
    expect(() => buildSpec(items[0]!, {}, project, 'snapshot')).toThrow(
      'Secret TOKEN has not been resolved',
    );
  });
});

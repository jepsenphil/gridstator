import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { mapRemotePath, matchProblems } from './problems';

let root: string;
const jobId = '01M6YVMX0DQRT0KCNNA4N7Z0J9';
beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'gst-problems-'));
  for (const f of ['src/index.ts', 'src/util.js', 'packages/web/app.tsx', 'test/math.test.js']) {
    await mkdir(join(root, f, '..'), { recursive: true });
    await writeFile(join(root, f), '');
  }
});
afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

describe('mapRemotePath', () => {
  it('maps container, act and relative paths to existing local files only', () => {
    const ctx = { root, workdir: '.', jobId };
    expect(mapRemotePath('/workspace/src/index.ts', ctx)).toBe(join(root, 'src/index.ts'));
    expect(
      mapRemotePath(`/home/x/.local/share/gridstator/agent/work/${jobId}/src/src/util.js`, ctx),
    ).toBe(join(root, 'src/util.js'));
    expect(mapRemotePath('app.tsx', { ...ctx, workdir: 'packages/web' })).toBe(
      join(root, 'packages/web/app.tsx'),
    );
    expect(mapRemotePath('/workspace/missing.ts', ctx)).toBeUndefined();
    expect(mapRemotePath('/etc/passwd', ctx)).toBeUndefined();
    expect(mapRemotePath('../outside.ts', ctx)).toBeUndefined();
  });
});

describe('matchProblems', () => {
  it('recognises tsc, file:line:col tools, eslint stylish and test stack traces', () => {
    const lines = [
      'src/index.ts(12,5): error TS2322: Type string is not assignable to type number.',
      '/workspace/src/util.js:3:10: warning: unused variable x',
      '/workspace/packages/web/app.tsx',
      '  4:7   error    Missing return type   @typescript-eslint/explicit-function-return-type',
      '  9:1   warning  Unexpected console    no-console',
      '',
      `[CI/test]   | ✖ adds numbers`,
      `[CI/test]   | AssertionError [ERR_ASSERTION]: Expected values to be strictly equal`,
      `[CI/test]   |     at TestContext.<anonymous> (/runner/work/${jobId}/src/test/math.test.js:8:10)`,
      'random/file.ts:1:1: error: not a local file',
      '\u001b[31msrc/index.ts(1,1): error TS1005: ";" expected.\u001b[0m',
    ];
    const found = matchProblems(lines, { root, workdir: '.', jobId });
    expect(
      found.map((p) => [p.file.slice(root.length + 1), p.line, p.column, p.severity, p.source]),
    ).toEqual([
      ['src/index.ts', 12, 5, 'error', 'tsc'],
      ['src/util.js', 3, 10, 'warning', 'remote'],
      ['packages/web/app.tsx', 4, 7, 'error', 'eslint'],
      ['packages/web/app.tsx', 9, 1, 'warning', 'eslint'],
      ['test/math.test.js', 8, 10, 'error', 'test'],
      ['src/index.ts', 1, 1, 'error', 'tsc'],
    ]);
    expect(found[2]?.code).toBe('@typescript-eslint/explicit-function-return-type');
    expect(found[4]?.message).toMatch(/AssertionError/);
  });
});

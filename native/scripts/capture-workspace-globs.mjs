#!/usr/bin/env node
// Supplemental source evidence. Node is used only to generate/check this development fixture.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { format } from 'prettier';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const baseline = JSON.parse(readFileSync(join(root, 'native/contracts/accepted-v1/baseline.json')));
const hash = (bytes) => `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
const sources = [
  'pnpm-lock.yaml',
  'packages/snapshot/src/capture.ts',
  'packages/snapshot/src/secrets.ts',
  'packages/protocol/src/manifest.ts',
];
const provenance = sources.map((path) => {
  const frozen = execFileSync('git', ['show', `${baseline.revision}:${path}`], { cwd: root });
  if (hash(frozen) !== hash(readFileSync(join(root, path))))
    throw Error(`Frozen source changed: ${path}`);
  return { path, sha256: hash(frozen) };
});
const require = createRequire(join(root, 'packages/snapshot/package.json'));
const picomatch = require('picomatch');
const version = require('picomatch/package.json').version;
if (version !== '4.0.7') throw Error(`Expected frozen picomatch4.0.7, found ${version}`);
execFileSync('pnpm', ['exec', 'tsc', '-b', 'packages/snapshot', '--force', '--pretty', 'false'], {
  cwd: root,
  stdio: 'inherit',
});
const { captureSnapshot } = await import(
  pathToFileURL(join(root, 'packages/snapshot/dist/capture.js'))
);
const { parseManifest } = await import(
  pathToFileURL(join(root, 'packages/protocol/dist/manifest.js'))
);
const manifestUnicodeCases = [
  ...[2048, 2049, 4096, 4097].map((count) => ({
    id: `symlink-target-${count}-astral`,
    field: 'target',
    count,
  })),
  ...[2048, 2049].map((count) => ({ id: `manifest-path-${count}-astral`, field: 'path', count })),
].map((test) => {
  const entry = {
    type: 'symlink',
    path: 'link',
    target: 'target',
    [test.field]: '💾'.repeat(test.count),
  };
  try {
    parseManifest(JSON.stringify({ version: 1, entries: [entry] }));
    return { ...test, accepted: true };
  } catch {
    return { ...test, accepted: false };
  }
});

const paths = [
  '',
  'README.md',
  '!README.md',
  '!!README.md',
  'src',
  'src/main.ts',
  'src/foo.ts',
  'src/bar.ts',
  'src/foobar.ts',
  'src/foofoo.ts',
  'src/.ts',
  'src/foo.test.ts',
  'src/foo.d.ts',
  'src/a.test.ts.ts',
  'src/fixtures/a.ts',
  'src/fixtures2/a.ts',
  'fixtures/a.ts',
  'fixtures2/a.ts',
  'tests/a.ts',
  'a/fixtures/x.ts',
  'a/fixtures2/x.ts',
  'a/other/x.ts',
  'a/b/c.ts',
  'main.ts',
  '.hidden.ts',
  'src/.hidden.ts',
  'src/a.ts',
  'x/fixtures3/x.ts',
  'x/.hidden/a.ts',
  'assets/foo.ts',
  'nested/safe.key',
  'foo',
  'bar',
  'foobar',
  'foofoo',
  'ba',
  'aba',
  'bb',
  'cab',
  '***',
  '*a',
  '+(*(foo))',
  '+(*(a)|foo)',
  'foo.ts',
  'foobar.ts',
  'foo.test.ts',
  'foo.d.ts',
  'a',
  'aa',
  'aaa',
  'ab',
  'b',
  '1',
  '2',
  '3',
  '10',
  '01',
  '02',
  '03',
  'abc',
  'abc/a',
  'abc/a/b',
  'filea.txt',
  'fileb.txt',
  'filec.txt',
  'file[ab].txt',
  'file[a-z].txt',
  'literal*.txt',
  'literalX.txt',
  'literal?.txt',
  'a{b',
  'a(b',
  '[',
  '[z-a]',
  'é',
  '💾',
  '😀',
  'é.ts',
  '💾.ts',
  '😀.ts',
  'xé.ts',
  'x💾.ts',
  '[é💾]',
  '.env',
  '.env.prod',
  '.env.example',
  'secret.key',
  'safe.key',
  'nested/secret.key',
];
const patterns = [
  ['leading-negation', ['!README.md']],
  ['double-negation', ['!!README.md']],
  ['triple-negation', ['!!!README.md']],
  ['array-or-not-unignore', ['README.md', '!README.md']],
  ['independent-negations', ['!README.md', '!src/**']],
  ['globstar-typescript', ['**/*.ts']],
  ['nonrecursive-star', ['*.ts']],
  ['tail-globstar', ['abc/**']],
  ['relative-pattern', ['./src/**/*.ts']],
  ['any-extglob', ['src/@(foo|bar).ts']],
  ['optional-extglob', ['src/?(foo|bar).ts']],
  ['zero-repeat-extglob', ['src/*(foo|bar).ts']],
  ['positive-repeat-extglob', ['src/+(foo|bar).ts']],
  ['negative-component-prefix', ['**/!(fixtures)/*.ts']],
  ['negative-alternatives', ['**/!(fixtures|tests)/*.ts']],
  ['negative-terminal', ['!(foo|bar)']],
  ['negative-literal-suffix', ['!(foo).ts']],
  ['negative-wildcard-suffix', ['**/!(*.test).ts']],
  ['negative-declaration-suffix', ['**/!(*.d).{ts,tsx}']],
  ['nested-positive', ['src/@(foo|@(bar|main)).ts']],
  ['negative-secrets', ['!**/secret.key']],
  ['negative-secret-extglob', ['**/!(secret).key']],
  ['brace-alternation', ['src/{foo,bar}.ts']],
  ['brace-empty', ['src/{,foo}.ts']],
  ['character-range', ['file[a-z].txt']],
  ['character-members-and-literal', ['file[ab].txt']],
  ['character-bang-is-literal', ['file[!ab].txt']],
  ['character-negation', ['file[^ab].txt']],
  ['posix-class', ['[[:digit:]]']],
  ['escaped-star', ['literal\\*.txt']],
  ['escaped-question', ['literal\\?.txt']],
  ['quoted-star', ['"literal*".txt']],
  ['raw-alternation-group', ['(foo|bar).ts']],
  ['numeric-range', ['{1..3}']],
  ['zero-padded-range-reference', ['{01..03}']],
  ['risky-repetition-reference', ['*(a|aa)']],
  ['nested-repeat-literalization', ['+(*(foo))']],
  ['nested-repeat-character-union', ['+(*(a)|*(b))']],
  ['nested-repeat-character-sequence', ['+(*(a)*(b))']],
  ['nested-repeat-noncombinable', ['+(*(a)|foo)']],
  ['nested-repeat-single-character-alias', ['+(*(@(a))|b)']],
  ['nested-repeat-nonbmp-literalization', ['+(*(💾)|a)']],
  ['nested-repeat-escaped-literal', ['+(*(\\*)|a)']],
  ['repeat-prefix-overlap', ['+(a|aa)']],
  ['repeat-wildcard-branch', ['+(*|a)']],
  ['literal-unclosed-bracket', ['[']],
  ['invalid-character-range', ['[z-a]']],
  ['literal-unclosed-parenthesis', ['a(b']],
  ['unclosed-brace-reference', ['a{b']],
  ['bmp-one-unit', ['?.ts']],
  ['nonbmp-two-units', ['??.ts']],
  ['bmp-class', ['[é].ts']],
  ['nonbmp-class', ['[💾].ts']],
  ['mixed-unicode-class', ['[é💾]']],
  ['unicode-positive-extglob', ['@(é|💾).ts']],
  ['unicode-negative-extglob', ['!(é).ts']],
  ['include-literal-base', ['README.md']],
  ['include-directory-base', ['src']],
];
const cases = patterns.map(([id, patterns]) => {
  const matcher = picomatch(patterns, { dot: true });
  return {
    id,
    patterns,
    bases: [...new Set(patterns.map((pattern) => picomatch.scan(pattern).base || ''))].sort(),
    matches: paths.filter((path) => matcher(path)),
  };
});

const rawPaths = [
  ...paths,
  'foofoo',
  'barbar',
  'foobar',
  'foobarbar',
  'aab',
  'abb',
  'abcabc',
  'foo/bar.ts',
  'bar/bar.ts',
  'foo/foo.ts',
  'a/b',
  'a/aa',
  'a/aaa',
  'foo\n',
  'a\n',
  'a\u0002',
  'a\u0000',
  'a8',
  'a+',
  'w+',
  'word',
  'WORD',
  '123',
  'éé',
  '💾💾',
  '😀😀',
  'é💾',
  '💾é',
  'foofoo.key',
  'src/foofoo.ts',
  'src/barbar.ts',
  '(?=foo)foo.ts',
  '(foo)\\1',
  '[\\d]',
  'aa.ts',
  'bb.ts',
  'abab.ts',
  'abab',
  'aaaab',
  'aaabaaa',
  'aaaa',
  'bbbb',
  'foobarbaz',
  'foobarbazbaz',
  ')',
  '))',
  '(',
  '((',
  '()',
  '[)]',
  '[1]',
  '\n',
  '\r',
  '\u2028',
  '\u2029',
  '\nfoo',
  'foo\n/bar.ts',
  'foo/\nbar.ts',
  'foo/bar.ts\n',
];
const rawPatterns = [
  '(?=foo)foo.ts',
  '(?!foo)*.ts',
  'foo/(?<=foo/)bar.ts',
  'foo/(?<!bar/)bar.ts',
  'a(?<=a+)b',
  'a(?<!b+)b',
  '(?<word>foo)\\k<word>',
  '(foo|bar)\\1',
  '@(foo|bar)\\1',
  '{foo,bar}\\1',
  '(?:(foo)|(bar))\\1\\2',
  '(?=(foo|foobar))\\1bar',
  '(foo)?\\1',
  '(foo|bar)?\\1',
  '((a)|b)\\1\\2',
  '(a(b))+\\1\\2',
  '(a|(b))+\\2',
  '\\1(a)',
  '(a)\\2',
  '(a)\\0',
  '(a)\\8',
  '(a)\\12',
  '(a)(b)\\2\\1',
  '(?<x>a)|(?<x>b)',
  '(?:(?<x>a)|(?<x>b))\\k<x>',
  '(?<word>foo)(?=\\k<word>)\\k<word>',
  '(?<x>é)\\k<x>',
  '(?<x>💾)\\k<x>',
  '(💾|😀)\\1',
  '(?=💾)??',
  '(?=💾)??.ts',
  '(?<=💾)??',
  '💾(?<=💾)é',
  '💾(?<!😀)é',
  '(?=(💾))\\1',
  '(?:foo|bar).ts',
  '(?:a|b)+',
  '(a+)',
  '(a)*',
  '(a)?',
  '(a?)',
  '[ab]+',
  '[ab]?',
  '[ab]*',
  '(\\w+)',
  '(\\d+)',
  '[\\d]+',
  '[^\\d]+',
  '[\\s]',
  '\\w+',
  '\\d',
  '\\bfoo\\b',
  '\\Bfoo',
  '\\u0061',
  '(\\u0061)',
  '\\x61',
  '(\\x61)',
  '(\\uD83D\\uDCBE)\\1',
  '([\\uD83D\\uDCBE])\\1',
  '([\\uD83D\\uDCBE]+)\\1',
  '\\uD83D\\uDCBE',
  '(foo)|bar',
  'src/foo|bar',
  '(?i:foo)',
  '(?<x>foo',
  '(?<x>a)(?<x>b)',
  '(?=foo',
  '(foo)\\k<missing>',
  'src/(foo|bar).ts',
  '**/(?=foo)foo.ts',
  '**/(foo|bar)\\1.ts',
  'src/@(foo|bar)\\1.ts',
  '!(foo|bar)\\1',
  '!(?=foo)foo.ts',
  '(?=foo)(foo).ts',
  '(?!(foo))*.ts',
  'aaa(?<=(a+))b\\1',
  '(?=(a+))a+\\1',
  '(?:(a)|(b))+\\1',
  '(a|@(b))\\2',
  '({a,b})\\1\\2',
  '([)])\\1',
  '([()])\\1',
  '(?=[)])*',
  '(?<x>[)])\\k<x>',
  '*',
  '**',
  '*.ts',
  '**/*.ts',
  'foo',
  'foo*',
  'foo/**',
  'foo/**/*.ts',
];
const rawCases = rawPatterns.map((pattern, index) => ({
  id: `raw-${index}`,
  patterns: [pattern],
  paths: rawPaths,
  regexSources: [picomatch.makeRe(pattern, { dot: true }).source],
  bases: [picomatch.scan(pattern).base || ''],
  matches: rawPaths.filter((path) => picomatch(pattern, { dot: true })(path)),
}));

const captureCases = [
  { id: 'ignored-literal-include', options: { include: ['README.md'] } },
  { id: 'ignored-negative-literal-include', options: { include: ['!README.md'] } },
  { id: 'ignored-negative-extglob-include', options: { include: ['!(README.md)'] } },
  { id: 'ignored-extglob-include', options: { include: ['src/@(foo|bar).ts'] } },
  { id: 'ignored-wildcard-include', options: { include: ['**/*.ts'] } },
  { id: 'negative-exclude', options: { exclude: ['!README.md'] } },
  { id: 'negative-extglob-exclude', options: { exclude: ['**/!(fixtures)/*.ts'] } },
  { id: 'secret-negated-allow', options: { allowSecrets: ['!secret.key'] } },
  { id: 'secret-extglob-allow', options: { allowSecrets: ['**/!(secret).key'] } },
  { id: 'raw-lookahead-include', options: { include: ['src/(?=foo)foo.ts'] } },
  { id: 'raw-lookahead-exclude', options: { exclude: ['(?=fixtures/)fixtures/*.ts'] } },
  { id: 'raw-lookahead-allow-secret', options: { allowSecrets: ['(?=safe.key)safe.key'] } },
];
const files = {
  '.gitignore': 'README.md\nsrc/\n',
  'README.md': 'readme',
  'other.md': 'other',
  'src/foo.ts': 'foo',
  'src/bar.ts': 'bar',
  'src/main.ts': 'main',
  'fixtures/a.ts': 'fixture',
  'fixtures2/a.ts': 'fixture2',
  '.env': 'credential',
  'secret.key': 'secret',
  'safe.key': 'safe',
};
for (const test of captureCases) {
  const directory = mkdtempSync(join(tmpdir(), 'gridstator-v1-globs-'));
  try {
    for (const [path, contents] of Object.entries(files)) {
      mkdirSync(dirname(join(directory, path)), { recursive: true });
      writeFileSync(join(directory, path), contents);
    }
    test.files = files;
    test.options = { noGit: true, ...test.options };
    const captured = await captureSnapshot({ root: directory, ...test.options });
    test.paths = captured.manifest.entries.map((entry) => entry.path);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}
const fixture = {
  formatVersion: 1,
  reference: {
    revision: baseline.revision,
    package: 'picomatch',
    version,
    options: { dot: true },
    sources: provenance,
  },
  paths,
  cases,
  rawCases,
  captureCases,
  manifestUnicodeCases,
  openCases: [],
  nativeResourceLimits: {
    maximumNesting: 128,
    maximumMatchWork: 2000000,
    behavior: 'Return an explicit capture error; never silently treat exhaustion as a non-match.',
  },
  invalid: [{ patterns: [''], error: 'Expected pattern to be a non-empty string' }],
};
const output = await format(JSON.stringify(fixture), {
  ...JSON.parse(readFileSync(join(root, '.prettierrc.json'))),
  parser: 'json',
});
const destination = join(root, 'native/contracts/workspace-globs-v1.json');
if (process.argv[2] === '--write') writeFileSync(destination, output);
else if (readFileSync(destination, 'utf8') !== output)
  throw Error('Workspace glob fixture differs from frozen source');
process.stdout.write(
  `${cases.length} matcher cases, ${captureCases.length} actual source captures verified against ${version}\n`,
);

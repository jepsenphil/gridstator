#!/usr/bin/env node
// AST inventory catches chained it.each/skipIf declarations without executing tests.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const reference = process.argv[2];
assert(reference, 'Usage: check-source-inventory.mjs FROZEN_CHECKOUT');
const baseline = JSON.parse(
  readFileSync(resolve(root, 'native/contracts/accepted-v1/baseline.json')),
);
const ledger = JSON.parse(readFileSync(resolve(root, 'native/contracts/parity-ledger.json')));
const indexed = new Set(
  ledger.rows.flatMap((row) => row.test.map((test) => `${test.path}:${test.line}`)),
);
const declared = [];
const wrappers = new Set([
  'each',
  'skipIf',
  'runIf',
  'only',
  'skip',
  'todo',
  'concurrent',
  'sequential',
  'fails',
]);
function testCallee(node) {
  if (ts.isIdentifier(node)) return node.text === 'it' || node.text === 'test';
  if (ts.isCallExpression(node) || ts.isTaggedTemplateExpression(node))
    return testCallee(node.expression);
  return (
    ts.isPropertyAccessExpression(node) &&
    wrappers.has(node.name.text) &&
    testCallee(node.expression)
  );
}
for (const source of baseline.sources) {
  const bytes = readFileSync(resolve(reference, source.path));
  assert.equal(
    `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
    source.sha256,
    source.path,
  );
  if (!/\.(?:test|spec)\.tsx?$/.test(source.path)) continue;
  const file = ts.createSourceFile(
    source.path,
    bytes.toString('utf8'),
    ts.ScriptTarget.Latest,
    true,
  );
  function visit(node) {
    if (
      ts.isCallExpression(node) &&
      testCallee(node.expression) &&
      node.arguments[0] &&
      ts.isStringLiteralLike(node.arguments[0])
    ) {
      const line = file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1;
      declared.push({ path: source.path, line, scenario: node.arguments[0].text });
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
}
const missing = declared.filter((test) => !indexed.has(`${test.path}:${test.line}`));
assert.deepEqual(missing, [], 'Frozen test declarations missing from native parity inventory');
process.stdout.write(
  `Verified ${declared.length} frozen test declarations, including chained conditional/parameterized cases, against ${ledger.rows.length} parity rows\n`,
);

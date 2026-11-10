#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const read = (path) => JSON.parse(readFileSync(resolve(root, path), 'utf8'));
const ledger = read('native/contracts/parity-ledger.json');
const ids = new Set();
const statuses = new Set(ledger.statusPolicy.allowed);
for (const row of ledger.rows) {
  assert(!ids.has(row.id), `Duplicate parity row: ${row.id}`);
  ids.add(row.id);
  for (const field of [
    'id',
    'family',
    'source',
    'test',
    'behavior',
    'owner',
    'acceptance',
    'status',
    'evidence',
  ]) {
    assert(Object.hasOwn(row, field), `${row.id}: missing ${field}`);
  }
  assert(statuses.has(row.status), `${row.id}: invalid status`);
  assert(
    row.behavior && row.owner && row.acceptance.description,
    `${row.id}: incomplete behavior/ownership/acceptance`,
  );
  for (const source of [...row.source, ...row.test.map((test) => test.path)]) {
    assert(existsSync(resolve(root, source)), `${row.id}: missing reference ${source}`);
  }
  assert(Array.isArray(row.evidence), `${row.id}: evidence must be an array`);
  if (row.status === 'verified' || (row.status === 'approved exception' && row.evidence.length)) {
    assert(row.evidence.length > 0, `${row.id}: verification requires evidence`);
    for (const proof of row.evidence) {
      for (const field of ['revision', 'command', 'result', 'environment', 'location']) {
        assert(proof[field], `${row.id}: evidence requires ${field}`);
      }
      assert.equal(proof.result, 'passed', `${row.id}: verified evidence must have passed`);
      assert(existsSync(resolve(root, proof.location)), `${row.id}: missing evidence location`);
    }
  }
  if (row.status === 'approved exception') {
    const exceptions = read('native/contracts/compatibility-exceptions.json').exceptions;
    assert(
      exceptions.some((exception) => exception.id === row.exceptionId),
      `${row.id}: missing approved exception`,
    );
  }
}
for (const filename of ['compatibility-exceptions.json', 'known-v1-limitations.json']) {
  const document = read(`native/contracts/${filename}`);
  for (const item of document.exceptions ?? document.limitations) {
    for (const source of item.reference ?? item.source) {
      assert(existsSync(resolve(root, source)), `${item.id}: missing ${source}`);
    }
  }
}
process.stdout.write(
  `Validated ${ids.size} parity rows; ${ledger.rows.filter((row) => row.status === 'verified').length} verified\n`,
);
if (process.argv.includes('--release')) {
  const open = ledger.rows.filter(
    (row) =>
      row.status !== 'verified' && !(row.status === 'approved exception' && row.evidence.length),
  );
  if (open.length) {
    process.stderr.write(
      `Stable v2 acceptance is incomplete: ${open.length} open rows.\n` +
        open
          .slice(0, 20)
          .map((row) => `  ${row.id}: ${row.status}\n`)
          .join('') +
        (open.length > 20
          ? `  ... ${open.length - 20} more; see native/contracts/parity-ledger.json\n`
          : ''),
    );
    process.exitCode = 1;
  }
}

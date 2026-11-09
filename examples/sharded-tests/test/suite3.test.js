import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checksum, work } from '../src/slow.js';

test('suite 3: checksum is stable', async () => {
  await work(1500);
  assert.equal(checksum('gridstator-3'), checksum('gridstator-3'));
});

test('suite 3: checksum differs between inputs', async () => {
  await work(1500);
  assert.notEqual(checksum('a3'), checksum('b3'));
});

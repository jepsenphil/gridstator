import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checksum, work } from '../src/slow.js';

test('suite 2: checksum is stable', async () => {
  await work(1500);
  assert.equal(checksum('gridstator-2'), checksum('gridstator-2'));
});

test('suite 2: checksum differs between inputs', async () => {
  await work(1500);
  assert.notEqual(checksum('a2'), checksum('b2'));
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checksum, work } from '../src/slow.js';

test('suite 4: checksum is stable', async () => {
  await work(1500);
  assert.equal(checksum('gridstator-4'), checksum('gridstator-4'));
});

test('suite 4: checksum differs between inputs', async () => {
  await work(1500);
  assert.notEqual(checksum('a4'), checksum('b4'));
});

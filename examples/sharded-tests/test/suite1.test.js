import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checksum, work } from '../src/slow.js';

test('suite 1: checksum is stable', async () => {
  await work(1500);
  assert.equal(checksum('gridstator-1'), checksum('gridstator-1'));
});

test('suite 1: checksum differs between inputs', async () => {
  await work(1500);
  assert.notEqual(checksum('a1'), checksum('b1'));
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { greet } from '../src/greet.js';

test('greet', () => {
  assert.equal(greet('Gridstator'), 'Hello, Gridstator!');
});

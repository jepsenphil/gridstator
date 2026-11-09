import assert from 'node:assert/strict';
import { test } from 'node:test';
import { add, fib } from '../src/math.js';

test('add', () => {
  assert.equal(add(2, 3), 5);
});

test('fib', () => {
  assert.equal(fib(20), 6765);
});

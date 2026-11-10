#!/usr/bin/env node
// Regenerate independently from the frozen TypeScript build, then compare JSON values. JSON
// formatting is immaterial; stdout/stderr and canonical-byte fields remain exact string values.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const reference = process.argv[2];
if (!reference) throw new Error('Usage: check-cli-protocol-fixtures.mjs FROZEN_CHECKOUT');
const native = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const temporary = mkdtempSync(join(tmpdir(), 'gridstator-cli-protocol-replay-'));
try {
  const captures = [
    ['capture-cli-registry.mjs', ['src/cli/commands.json']],
    [
      'capture-cli-acceptance.mjs',
      ['contracts/cli-native-help-v2.json', 'contracts/cli-process-boundaries-v1.json'],
    ],
    ['capture-protocol-boundaries.mjs', ['contracts/protocol-boundaries-v1.json']],
    ['capture-cli-workflows.mjs', ['contracts/cli-workflows-v1.json']],
    ['capture-client-urls.mjs', ['contracts/client-urls-v1.json']],
    ['capture-update-compatibility.mjs', ['contracts/update-compatibility-v1.json']],
  ];
  let count = 0;
  for (const [script, paths] of captures) {
    const outputs = paths.map((_, index) => join(temporary, `${count}-${index}.json`));
    execFileSync(
      process.execPath,
      [join(native, 'scripts', script), resolve(reference), ...outputs],
      { stdio: 'pipe' },
    );
    for (let index = 0; index < paths.length; index++) {
      assert.deepStrictEqual(
        JSON.parse(readFileSync(outputs[index], 'utf8')),
        JSON.parse(readFileSync(join(native, paths[index]), 'utf8')),
        paths[index],
      );
      count++;
    }
  }
  process.stdout.write(
    `Verified ${count} supplementary CLI/protocol fixture and registry documents against frozen TypeScript\n`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

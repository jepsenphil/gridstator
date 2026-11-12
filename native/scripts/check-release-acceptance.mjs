#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const document = JSON.parse(
  readFileSync(resolve(root, 'native/contracts/release-acceptance.json'), 'utf8'),
);
const expected = new Map([
  ['mac-signing', 'mac-stable-future'],
  ['linux-two-target-runtime', 'linux-stable'],
  ['mac-docker-capacity', 'mac-stable-future'],
  ['launchd-live', 'mac-stable-future'],
  ['systemd-live', 'linux-stable'],
]);
const release = process.argv.includes('--release');
const revisionIndex = process.argv.indexOf('--revision');
const revision = revisionIndex >= 0 ? process.argv[revisionIndex + 1] : undefined;
const candidateIndex = process.argv.indexOf('--candidate-revision');
const candidateRevision = candidateIndex >= 0 ? process.argv[candidateIndex + 1] : undefined;
if (release) {
  assert.match(revision ?? '', /^[a-f0-9]{40}$/, '--release requires --revision <40-hex commit>');
  assert.match(
    candidateRevision ?? '',
    /^[a-f0-9]{40}$/,
    '--release requires --candidate-revision <40-hex commit>',
  );
  assert(sourceEquivalent(candidateRevision), 'candidate source differs from release revision');
}
const releaseSources = [
  'package.json',
  'pnpm-lock.yaml',
  'apps/vscode',
  'packages',
  'native/Cargo.toml',
  'native/Cargo.lock',
  'native/build.rs',
  'native/src',
  'native/tests',
  'native/examples',
  'native/scripts',
  'native/contracts',
  ':(exclude)native/contracts/evidence',
  ':(exclude)native/contracts/release-acceptance.json',
  'scripts',
  '.github/workflows/native-candidate.yml',
  '.github/workflows/native-publish.yml',
];
function sourceEquivalent(proofRevision) {
  if (proofRevision === revision) return true;
  try {
    execFileSync('git', ['merge-base', '--is-ancestor', proofRevision, revision], { cwd: root });
    execFileSync('git', ['diff', '--quiet', proofRevision, revision, '--', ...releaseSources], {
      cwd: root,
    });
    return true;
  } catch {
    return false;
  }
}

assert.equal(document.schemaVersion, 1);
assert.equal(document.releaseSeries, '2.x');
assert(Array.isArray(document.gates));
assert.equal(document.gates.length, expected.size);
for (const gate of document.gates) {
  assert.equal(expected.get(gate.id), gate.scope, `Unknown or duplicate native gate: ${gate.id}`);
  expected.delete(gate.id);
  assert(document.statusPolicy.includes(gate.status), `${gate.id}: invalid status`);
  assert(gate.owner && gate.behavior && gate.acceptance?.description);
  assert(Array.isArray(gate.evidence));
  for (const proof of gate.evidence) {
    for (const field of ['revision', 'command', 'result', 'environment', 'location'])
      assert(proof[field], `${gate.id}: evidence requires ${field}`);
    assert.equal(proof.result, 'passed', `${gate.id}: evidence must have passed`);
    assert(existsSync(resolve(root, proof.location)), `${gate.id}: missing ${proof.location}`);
  }
  if (gate.status === 'verified') {
    assert(gate.evidence.length > 0, `${gate.id}: verification requires evidence`);
    assert(
      gate.evidence.some((proof) => /^[a-f0-9]{40}$/.test(proof.revision)),
      `${gate.id}: final verification requires an exact source commit`,
    );
    if (release && gate.scope === 'linux-stable')
      assert(
        gate.evidence.some(
          (proof) => proof.revision === candidateRevision && sourceEquivalent(proof.revision),
        ),
        `${gate.id}: no passing evidence from source equivalent to ${revision}`,
      );
  }
}
assert.equal(expected.size, 0, `Missing native gates: ${[...expected.keys()].join(', ')}`);

const open = document.gates.filter(
  (gate) => gate.scope === 'linux-stable' && gate.status !== 'verified',
);
process.stdout.write(
  `Validated ${document.gates.length} native gates; ${open.length} Linux stable gates open\n`,
);
if (release && open.length) {
  process.stderr.write(
    `Stable Linux release acceptance is incomplete:\n${open
      .map((gate) => `  ${gate.id}: ${gate.status}`)
      .join('\n')}\n`,
  );
  process.exitCode = 1;
}

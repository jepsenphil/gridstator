#!/usr/bin/env node
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
const reference = resolve(process.argv[2] ?? '');
const output = process.argv[3];
if (!output) throw new Error('Usage: capture-update-compatibility.mjs FROZEN_CHECKOUT OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216') throw new Error('Wrong reference');
const { GridstatorClient } = await import(
  pathToFileURL(join(reference, 'packages/client/dist/client.js'))
);
const { StartUpdateRequestSchema } = await import(
  pathToFileURL(join(reference, 'packages/protocol/dist/update.js'))
);
const { GridstatorError } = await import(
  pathToFileURL(join(reference, 'packages/shared/dist/errors.js'))
);
const legacySchema = StartUpdateRequestSchema.extend({
  drainTimeoutSeconds: StartUpdateRequestSchema.shape.drainTimeoutSeconds
    .unwrap()
    .unwrap()
    .default(3600),
});
const view = {
  controller: { id: 'old', version: '0.1.0', sha256: null, updatable: true, reason: null },
  autoUpdate: 'off',
  updateWindow: null,
  rollout: null,
};
const specifications = [
  {
    id: 'reference-controller-update-compatibility-001',
    input: { now: false, drainTimeoutSeconds: null },
    legacy: true,
  },
  { id: 'reference-controller-update-compatibility-002', input: {}, legacy: true },
  { id: 'reference-controller-update-compatibility-003', input: {} },
  {
    id: 'reference-controller-update-compatibility-004',
    input: { now: false, drainTimeoutSeconds: null },
  },
  {
    id: 'reference-controller-update-compatibility-005',
    input: { nodes: ['worker'], drainTimeoutSeconds: 90 },
    legacy: true,
  },
  {
    id: 'reference-controller-update-compatibility-006',
    input: { now: true, drainTimeoutSeconds: null },
    legacy: true,
  },
  {
    id: 'reference-controller-update-compatibility-007',
    input: { drainTimeoutSeconds: null },
    forbidden: true,
  },
];
const errorView = (error) => ({
  code: error.code,
  message: error.message,
  ...(error.hint ? { hint: error.hint } : {}),
  ...(error.cause ? { cause: errorView(error.cause) } : {}),
});
const cases = [];
for (const specification of specifications) {
  const requests = [];
  const client = new GridstatorClient({
    url: 'https://oracle.invalid',
    ca: '',
    token: 'test-only',
  });
  let response;
  // Invoke the frozen public startUpdate implementation. Its private transport
  // is replaced only to capture the exact serialized body and source errors.
  client.request = async (method, path, options) => {
    const body = JSON.parse(JSON.stringify(options.json));
    requests.push({ method, path, body });
    const parsed = (specification.legacy ? legacySchema : StartUpdateRequestSchema).safeParse(body);
    let error = specification.forbidden
      ? new GridstatorError('FORBIDDEN', 'Admin access required')
      : undefined;
    if (!parsed.success)
      error = new GridstatorError(
        'BAD_REQUEST',
        `Invalid update request: ${parsed.error.issues.map((issue) => `${issue.path.join('.')}: ${issue.message}`).join('; ')}`,
      );
    response = error ? { status: error.status, body: error.toBody() } : { status: 200, body: view };
    if (error) throw error;
    return view;
  };
  let result;
  try {
    result = { value: await client.startUpdate(specification.input) };
  } catch (error) {
    result = { error: errorView(error) };
  } finally {
    client.close();
  }
  cases.push({ ...specification, requests, response, ...result });
}
const paths = ['packages/client/src/client.ts', 'apps/controller/src/update-compatibility.test.ts'];
writeFileSync(
  output,
  `${JSON.stringify(
    {
      formatVersion: 1,
      sourceRevision: revision,
      source: paths.map((path) => ({
        path,
        sha256: createHash('sha256')
          .update(readFileSync(join(reference, path)))
          .digest('hex'),
      })),
      nativeException:
        'The recovery hint names a native release bundle instead of the retired Node package.',
      cases,
    },
    null,
    2,
  )}\n`,
);

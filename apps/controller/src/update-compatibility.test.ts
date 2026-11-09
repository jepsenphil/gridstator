import { mkdtemp, rm } from 'node:fs/promises';
import { createServer, type Server } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { z } from 'zod';
import { GridstatorClient } from '@gridstator/client';
import { StartUpdateRequestSchema, type UpdatesView } from '@gridstator/protocol';
import { GridstatorError } from '@gridstator/shared';
import { generatePki } from './pki.js';

// The controller shipped before indefinite draining only accepts a numeric deadline.
const legacySchema = StartUpdateRequestSchema.extend({
  drainTimeoutSeconds: z
    .number()
    .int()
    .positive()
    .max(7 * 24 * 3600)
    .default(3600),
});
const view: UpdatesView = {
  controller: { id: 'old', version: '0.1.0', sha256: null, updatable: true, reason: null },
  autoUpdate: 'off',
  updateWindow: null,
  rollout: null,
};

let directory: string;
let server: Server;
let client: GridstatorClient;
let schema: z.ZodType;
let failure: GridstatorError | undefined;
let requests: unknown[];

beforeEach(async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-update-compatibility-'));
  const pki = await generatePki(directory, 'test-cluster');
  schema = legacySchema;
  failure = undefined;
  requests = [];
  server = createServer({ key: pki.serverKeyPem, cert: pki.serverChainPem }, (req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => {
      const body: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
      requests.push(body);
      const parsed = schema.safeParse(body);
      let error = failure;
      if (!parsed.success) {
        const issues = parsed.error.issues
          .map((issue) => `${issue.path.join('.')}: ${issue.message}`)
          .join('; ');
        error = new GridstatorError('BAD_REQUEST', `Invalid update request: ${issues}`);
      }
      res.writeHead(error?.status ?? 200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(error?.toBody() ?? view));
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No listening address');
  client = new GridstatorClient({
    url: `https://127.0.0.1:${address.port}`,
    ca: pki.caPem,
    token: 'test-admin',
  });
});

afterEach(async () => {
  client.close();
  await new Promise<void>((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  await rm(directory, { recursive: true, force: true });
});

describe('worker update compatibility', () => {
  it('explains how to recover when an older controller rejects indefinite draining', async () => {
    await expect(
      client.startUpdate({ now: false, drainTimeoutSeconds: null }),
    ).rejects.toMatchObject({
      code: 'CONFLICT',
      message: expect.stringContaining('Update the controller first'),
      cause: { code: 'BAD_REQUEST', message: expect.stringContaining('received null') },
    });
    // Retrying with an omitted deadline would silently enable the legacy one-hour cancellation.
    expect(requests).toEqual([{ now: false, drainTimeoutSeconds: null }]);
  });

  it('keeps the default request from silently enabling legacy job cancellation', async () => {
    await expect(client.startUpdate()).rejects.toMatchObject({ code: 'CONFLICT' });
    expect(requests).toEqual([{ drainTimeoutSeconds: null }]);
  });

  it('sends an explicit indefinite deadline to a current controller by default', async () => {
    schema = StartUpdateRequestSchema;
    await expect(client.startUpdate()).resolves.toEqual(view);
    expect(requests).toEqual([{ drainTimeoutSeconds: null }]);
  });

  it('accepts the dashboard request on a current controller', async () => {
    schema = StartUpdateRequestSchema;
    await expect(client.startUpdate({ now: false, drainTimeoutSeconds: null })).resolves.toEqual(
      view,
    );
    expect(requests).toEqual([{ now: false, drainTimeoutSeconds: null }]);
  });

  it('preserves an explicit deadline for older controllers', async () => {
    await expect(
      client.startUpdate({ nodes: ['worker'], drainTimeoutSeconds: 90 }),
    ).resolves.toEqual(view);
    expect(requests).toEqual([{ nodes: ['worker'], drainTimeoutSeconds: 90 }]);
  });

  it('allows an explicitly immediate update on an older controller', async () => {
    await expect(client.startUpdate({ now: true, drainTimeoutSeconds: null })).resolves.toEqual(
      view,
    );
    expect(requests).toEqual([{ now: true }]);
  });

  it('preserves unrelated controller errors', async () => {
    failure = new GridstatorError('FORBIDDEN', 'Admin access required');
    schema = StartUpdateRequestSchema;
    await expect(client.startUpdate({ drainTimeoutSeconds: null })).rejects.toMatchObject({
      code: 'FORBIDDEN',
      message: 'Admin access required',
    });
    expect(requests).toHaveLength(1);
  });
});

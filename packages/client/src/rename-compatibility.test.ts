import { mkdtemp, rm } from 'node:fs/promises';
import { createServer, type Server } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, expect, it } from 'vitest';
import { NodeSchema, defaultNodeSettings } from '@gridstator/protocol';
import { generatePki } from '../../../apps/controller/src/pki.js';
import { GridstatorClient } from './client.js';

let directory: string | undefined;
let server: Server | undefined;
let client: GridstatorClient | undefined;
afterEach(async () => {
  client?.close();
  if (server)
    await new Promise<void>((resolve, reject) =>
      server!.close((error) => (error ? reject(error) : resolve())),
    );
  if (directory) await rm(directory, { recursive: true, force: true });
});

it('refuses an older controller that ignores the name field rather than reporting a successful rename', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-rename-compatibility-'));
  const pki = await generatePki(directory, 'rename-test');
  const original = NodeSchema.parse({
    id: 'node-id',
    name: 'DESKTOP-JEL',
    status: 'offline',
    health: 'unknown',
    healthIssues: [],
    info: null,
    metrics: null,
    settings: defaultNodeSettings({ cpus: 2, memoryTotalMb: 4096 }),
    effectiveLabels: [],
    activeJobs: [],
    allocated: { cpus: 0, memoryMb: 0 },
    allocatable: { cpus: 1, memoryMb: 2048 },
    lastSeenAt: null,
    connectedAt: null,
    createdAt: 1,
  });
  let body: unknown;
  server = createServer(
    { key: pki.serverKeyPem, cert: pki.serverChainPem },
    (request, response) => {
      const chunks: Buffer[] = [];
      request.on('data', (chunk: Buffer) => chunks.push(chunk));
      request.on('end', () => {
        body = JSON.parse(Buffer.concat(chunks).toString());
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify(original));
      });
    },
  );
  await new Promise<void>((resolve) => server!.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Expected TCP listener');
  client = new GridstatorClient({
    url: `https://127.0.0.1:${address.port}`,
    ca: pki.caPem,
    token: 'test-admin',
  });
  await expect(client.updateNode('DESKTOP-JEL', { name: 'big-box' })).rejects.toMatchObject({
    code: 'UNAVAILABLE',
    message: expect.stringMatching(/update the controller/i),
  });
  expect(body).toEqual({ name: 'big-box' });
  expect((await client.updateNode('DESKTOP-JEL', {})).name).toBe('DESKTOP-JEL');
});

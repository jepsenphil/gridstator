import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, expect, it } from 'vitest';
import { initController, startController, type RunningController } from '@gridstator/controller';
import { GridstatorClient, watchClientConfig, writeClientConfig } from '@gridstator/client';
import { ClusterState } from './state';

let directory: string;
let controller: RunningController | undefined;
const windows: ClusterState[] = [];
const disposers: Array<() => void> = [];
const clients: GridstatorClient[] = [];
afterEach(async () => {
  disposers.splice(0).forEach((dispose) => dispose());
  windows.splice(0).forEach((window) => window.dispose());
  clients.splice(0).forEach((client) => client.close());
  await controller?.close();
  controller = undefined;
  if (directory) await rm(directory, { recursive: true, force: true });
});

it('three open SSH windows connect on first shared login and adopt rotated credentials', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-extension-windows-'));
  const init = await initController({ dataDir: directory, host: '127.0.0.1', port: 0 });
  controller = await startController({ dataDir: directory, host: '127.0.0.1', port: 0 });
  const path = join(directory, 'client.json');
  for (let i = 0; i < 3; i++) {
    const state = new ClusterState(
      () => path,
      () => 100,
      { appendLine: () => undefined },
    );
    windows.push(state);
    await state.connect();
    expect(state.connection.status).toBe('signedOut');
    disposers.push(
      watchClientConfig(
        path,
        () => {
          void state.connect();
        },
        25,
      ),
    );
  }
  const base = {
    version: 1 as const,
    url: controller.url,
    caPem: init.caPem,
    caFingerprint: init.fingerprint,
  };
  await writeClientConfig({ ...base, token: init.adminToken }, path);
  await expect
    .poll(() => windows.map((w) => w.connection.status))
    .toEqual(['connected', 'connected', 'connected']);
  const client = new GridstatorClient({
    url: controller.url,
    ca: init.caPem,
    token: init.adminToken,
  });
  clients.push(client);
  const rotated = await client.createToken({
    kind: 'client',
    name: 'shared-after-rotation',
    role: 'admin',
  });
  await writeClientConfig({ ...base, token: rotated.token }, path);
  await expect
    .poll(() => windows.map((w) => w.connection.user))
    .toEqual(['shared-after-rotation', 'shared-after-rotation', 'shared-after-rotation']);
  expect(
    windows.every(
      (w) =>
        w.connection.controller?.hostname && w.connection.version && w.connection.caFingerprint,
    ),
  ).toBe(true);
});

it('revoked shared credentials stop reconnecting and expose local access restoration', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-extension-revoked-'));
  const init = await initController({ dataDir: directory, host: '127.0.0.1', port: 0 });
  controller = await startController({ dataDir: directory, host: '127.0.0.1', port: 0 });
  const client = new GridstatorClient({
    url: controller.url,
    ca: init.caPem,
    token: init.adminToken,
  });
  clients.push(client);
  const token = await client.createToken({ kind: 'client', name: 'window-admin', role: 'admin' });
  const path = join(directory, 'client.json');
  await writeClientConfig(
    {
      version: 1,
      url: controller.url,
      caPem: init.caPem,
      caFingerprint: init.fingerprint,
      token: token.token,
    },
    path,
  );
  let preparations = 0;
  const state = new ClusterState(
    () => path,
    () => 100,
    { appendLine: () => undefined },
    async () => {
      preparations++;
    },
    (failed) => ({ canRestoreLocalAccess: failed }),
  );
  windows.push(state);
  await state.connect();
  expect(state.connected).toBe(true);
  await client.revokeToken(token.info.id);
  await state.connect();
  expect(state.connection.status).toBe('error');
  expect(state.connection.canRestoreLocalAccess).toBe(true);
  const count = preparations;
  await new Promise((resolve) => setTimeout(resolve, 1100));
  expect(preparations).toBe(count);
});

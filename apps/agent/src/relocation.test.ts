import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { GridstatorClient } from '@gridstator/client';
import { initController, startController, type RunningController } from '@gridstator/controller';
import { relocateController } from './relocation.js';
import { readAgentState, writeAgentState, type AgentState } from './state.js';

let dir: string;
const controllers: RunningController[] = [];
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'gst-move-'));
});
afterEach(async () => {
  await Promise.all(controllers.splice(0).map((c) => c.close()));
  await rm(dir, { recursive: true, force: true });
});

async function createController(name: string) {
  const dataDir = join(dir, name);
  const initial = await initController({ dataDir, host: '127.0.0.1', port: 0 });
  const controller = await startController({ dataDir });
  controllers.push(controller);
  return { initial, controller };
}

describe('controller relocation', () => {
  it('verifies pinned TLS, persists the move and preserves a fallback before retargeting shared clients', async () => {
    const { initial, controller } = await createController('controller');
    const state: AgentState = {
      version: 1,
      controllerUrl: 'https://127.0.0.1:1',
      caPem: initial.caPem,
      caFingerprint: initial.fingerprint,
      nodeId: 'worker',
      name: 'worker',
      credential: initial.adminToken,
    };
    const client = new GridstatorClient({
      url: state.controllerUrl,
      ca: state.caPem,
      token: state.credential,
    });
    try {
      expect(await relocateController(join(dir, 'agent'), state, client, controller.url)).toBe(
        true,
      );
      expect(client.url).toBe(controller.url);
      expect((await readAgentState(join(dir, 'agent')))?.controllerFallbackUrls).toContain(
        'https://127.0.0.1:1',
      );
      expect((await client.health()).ok).toBe(true);
    } finally {
      client.close();
    }
  });

  it('refuses another cluster without changing memory or the saved endpoint', async () => {
    const a = await createController('a');
    const b = await createController('b');
    const state: AgentState = {
      version: 1,
      controllerUrl: a.controller.url,
      caPem: a.initial.caPem,
      caFingerprint: a.initial.fingerprint,
      nodeId: 'worker',
      name: 'worker',
      credential: a.initial.adminToken,
    };
    const dataDir = join(dir, 'agent');
    await writeAgentState(dataDir, state);
    const client = new GridstatorClient({
      url: state.controllerUrl,
      ca: state.caPem,
      token: state.credential,
    });
    try {
      await expect(
        relocateController(dataDir, state, client, b.controller.url),
      ).rejects.toMatchObject({ code: 'TLS_PIN_MISMATCH' });
      expect(client.url).toBe(a.controller.url);
      expect(await readAgentState(dataDir)).toEqual(state);
    } finally {
      client.close();
    }
  });
});

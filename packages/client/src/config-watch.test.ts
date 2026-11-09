import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, expect, it } from 'vitest';
import { readClientConfig, watchClientConfig, writeClientConfig } from './config.js';

let directory: string | undefined;
const stops: Array<() => void> = [];
afterEach(async () => {
  for (const stop of stops.splice(0)) stop();
  if (directory) await rm(directory, { recursive: true, force: true });
});

it('three open clients observe first login, atomic credential replacement and deletion', async () => {
  directory = await mkdtemp(join(tmpdir(), 'gridstator-windows-'));
  const path = join(directory, 'nested', 'client.json');
  const observed: Array<string | undefined> = [undefined, undefined, undefined];
  const counts = [0, 0, 0];
  for (let i = 0; i < 3; i++) {
    stops.push(
      watchClientConfig(
        path,
        () => {
          counts[i] = counts[i]! + 1;
          void readClientConfig(path).then((c) => {
            observed[i] = c?.token;
          });
        },
        25,
      ),
    );
  }
  const base = {
    version: 1 as const,
    url: 'https://controller:7300',
    caPem: 'ca',
    caFingerprint: 'sha256:fixture',
  };
  await writeClientConfig({ ...base, token: 'first-shared-login' }, path);
  await expect
    .poll(() => observed)
    .toEqual(['first-shared-login', 'first-shared-login', 'first-shared-login']);
  await writeClientConfig({ ...base, token: 'rotated-shared-login' }, path);
  await expect
    .poll(() => observed)
    .toEqual(['rotated-shared-login', 'rotated-shared-login', 'rotated-shared-login']);
  await rm(path);
  await expect.poll(() => observed).toEqual([undefined, undefined, undefined]);
  expect(counts.every((n) => n >= 3)).toBe(true);
  stops.splice(0).forEach((stop) => stop());
  const before = [...counts];
  await writeClientConfig({ ...base, token: 'after-disposal' }, path);
  await new Promise((resolve) => setTimeout(resolve, 75));
  expect(counts).toEqual(before);
});

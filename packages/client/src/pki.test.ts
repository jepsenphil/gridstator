import { X509Certificate } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer, request } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { generatePki } from '../../../apps/controller/src/pki.js';
import { fetchPinnedCa } from './pki.js';

it('bootstrap trusts only the fingerprinted CA when an attacker appends another root', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'gridstator-pinned-ca-'));
  const trusted = await generatePki(join(directory, 'trusted'), 'trusted');
  const attacker = await generatePki(join(directory, 'attacker'), 'attacker');
  let credentialRequests = 0;
  const server = createServer(
    { cert: attacker.serverChainPem, key: attacker.serverKeyPem },
    (req, res) => {
      if (req.url === '/v1/pki/ca.pem') res.end(trusted.caPem + attacker.caPem);
      else {
        credentialRequests++;
        res.end('unexpected authenticated request');
      }
    },
  );
  try {
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('expected TCP listener');
    const url = `https://127.0.0.1:${address.port}`;
    const ca = await fetchPinnedCa(url, trusted.fingerprint);
    expect(ca).toBe(new X509Certificate(trusted.caPem).toString());
    await expect(
      new Promise<void>((resolve, reject) => {
        request(
          `${url}/v1/cluster`,
          { ca, headers: { Authorization: 'Bearer test-secret' } },
          (res) => {
            res.resume();
            resolve();
          },
        )
          .on('error', reject)
          .end();
      }),
    ).rejects.toThrow();
    expect(credentialRequests).toBe(0);
  } finally {
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
    await rm(directory, { recursive: true, force: true });
  }
});

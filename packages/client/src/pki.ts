import { X509Certificate } from 'node:crypto';
import { GridstatorError, certificateFingerprint, normalizeFingerprint } from '@gridstator/shared';
import { networkError, rawRequest, readBody } from './http.js';

/**
 * First-contact CA retrieval. Downloads the cluster CA without TLS verification, then accepts it
 * only if its SHA-256 fingerprint matches the one the user supplied out of band. All later
 * connections trust exactly this CA.
 */
export async function fetchPinnedCa(baseUrl: string, fingerprint: string): Promise<string> {
  const expected = normalizeFingerprint(fingerprint);
  let pem: string;
  try {
    const res = await rawRequest({
      method: 'GET',
      url: `${baseUrl}/v1/pki/ca.pem`,
      agent: false,
      rejectUnauthorized: false,
      timeoutMs: 10_000,
    });
    if (res.statusCode !== 200) {
      res.resume();
      throw new GridstatorError(
        'UNAVAILABLE',
        `${baseUrl} did not serve a cluster CA (HTTP ${res.statusCode})`,
        {
          hint: 'Is this a Gridstator controller URL?',
        },
      );
    }
    pem = (await readBody(res, 64 * 1024)).toString('utf8');
  } catch (err) {
    throw networkError(baseUrl, err);
  }
  let actual: string;
  try {
    // The fingerprint covers one certificate. Never pass additional, unpinned roots from the
    // bootstrap response to later TLS connections (including VS Code's live API client).
    pem = new X509Certificate(pem).toString();
    actual = certificateFingerprint(pem);
  } catch {
    throw new GridstatorError('TLS_PIN_MISMATCH', `${baseUrl} returned an invalid CA certificate`);
  }
  if (actual !== expected) {
    throw new GridstatorError(
      'TLS_PIN_MISMATCH',
      `The controller's CA fingerprint ${actual} does not match the expected ${expected}`,
      {
        hint: 'Copy the fingerprint printed by `gridstator controller init` / `gridstator token create` exactly. A mismatch can indicate an interception attempt.',
      },
    );
  }
  return pem;
}

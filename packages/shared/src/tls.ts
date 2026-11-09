import { X509Certificate, createHash } from 'node:crypto';

/** SHA-256 fingerprint of a PEM certificate's DER encoding, formatted `sha256:<hex>`. */
export function certificateFingerprint(pem: string): string {
  const cert = new X509Certificate(pem);
  return 'sha256:' + createHash('sha256').update(cert.raw).digest('hex');
}

/**
 * Accepts `sha256:<hex>`, bare hex, or colon-separated hex (as printed by openssl) and returns the
 * canonical `sha256:<lowercase hex>` form.
 */
export function normalizeFingerprint(input: string): string {
  const stripped = input
    .trim()
    .replace(/^sha256:/i, '')
    .replace(/:/g, '')
    .toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(stripped)) {
    throw new Error(`Invalid fingerprint "${input}" (expected sha256:<64 hex chars>)`);
  }
  return 'sha256:' + stripped;
}

/** Short, human-friendly form for display (first 16 hex chars). */
export function shortFingerprint(fp: string): string {
  return fp.replace(/^sha256:/, '').slice(0, 16);
}

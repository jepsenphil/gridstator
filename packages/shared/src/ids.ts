import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/**
 * Generates a ULID-style identifier: 10 chars of millisecond timestamp + 16 chars of randomness,
 * Crockford base32. Lexicographic order matches creation order (to the millisecond).
 */
export function newId(now: number = Date.now()): string {
  let time = now;
  let timePart = '';
  for (let i = 0; i < 10; i++) {
    timePart = CROCKFORD[time % 32] + timePart;
    time = Math.floor(time / 32);
  }
  const bytes = randomBytes(16);
  let randPart = '';
  for (let i = 0; i < 16; i++) {
    randPart += CROCKFORD[(bytes[i] ?? 0) % 32];
  }
  return timePart + randPart;
}

export type TokenKind = 'client' | 'join' | 'agent';

const TOKEN_PREFIX: Record<TokenKind, string> = {
  client: 'gsc_',
  join: 'gsj_',
  agent: 'gsa_',
};

/** Creates a 256-bit random bearer token with a type prefix. */
export function newToken(kind: TokenKind): string {
  return TOKEN_PREFIX[kind] + randomBytes(32).toString('base64url');
}

export function tokenKind(token: string): TokenKind | undefined {
  for (const [kind, prefix] of Object.entries(TOKEN_PREFIX)) {
    if (token.startsWith(prefix)) return kind as TokenKind;
  }
  return undefined;
}

export function sha256Hex(data: string | Uint8Array): string {
  return createHash('sha256').update(data).digest('hex');
}

/** Tokens are high-entropy, so an unsalted SHA-256 is an appropriate at-rest representation. */
export function hashToken(token: string): string {
  return sha256Hex(token);
}

export function constantTimeEqualHex(a: string, b: string): boolean {
  const ab = Buffer.from(a, 'hex');
  const bb = Buffer.from(b, 'hex');
  return ab.length === bb.length && timingSafeEqual(ab, bb);
}

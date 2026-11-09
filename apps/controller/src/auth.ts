import { GridstatorError, hashToken, newId, newToken, tokenKind } from '@gridstator/shared';
import type { TokenRole, TokenView } from '@gridstator/protocol';
import type { Store, TokenRow } from './db.js';

export interface Principal {
  tokenId: string;
  kind: 'client' | 'agent' | 'join';
  name: string;
  role: TokenRole | null;
  nodeId: string | null;
}

export type AuthRequirement = 'none' | 'client' | 'admin' | 'agent' | 'client-or-agent' | 'join';

export function tokenView(t: TokenRow): TokenView {
  return {
    id: t.id,
    kind: t.kind,
    name: t.name,
    role: t.role,
    nodeId: t.node_id,
    createdAt: t.created_at,
    expiresAt: t.expires_at,
    usesRemaining: t.uses_remaining,
    lastUsedAt: t.last_used_at,
    revokedAt: t.revoked_at,
  };
}

export class Auth {
  private readonly store: Store;
  private readonly lastTouched = new Map<string, number>();

  constructor(store: Store) {
    this.store = store;
  }

  /** Creates and persists a token; returns the secret (shown once) and its stored row. */
  issue(input: {
    kind: 'client' | 'join' | 'agent';
    name: string;
    role?: TokenRole | null;
    nodeId?: string | null;
    expiresAt?: number | null;
    usesRemaining?: number | null;
  }): { token: string; row: TokenRow } {
    const token = newToken(input.kind);
    const row: TokenRow = {
      id: newId(),
      kind: input.kind,
      name: input.name,
      hash: hashToken(token),
      role: input.role ?? null,
      node_id: input.nodeId ?? null,
      created_at: Date.now(),
      expires_at: input.expiresAt ?? null,
      uses_remaining: input.usesRemaining ?? null,
      last_used_at: null,
      revoked_at: null,
    };
    this.store.insertToken(row);
    return { token, row };
  }

  /** Validates a bearer token against a requirement. Throws UNAUTHORIZED/FORBIDDEN. */
  authenticate(header: string | undefined, requirement: AuthRequirement): Principal | undefined {
    if (requirement === 'none') return undefined;
    const match = /^Bearer\s+(\S+)$/i.exec(header ?? '');
    if (!match) {
      throw new GridstatorError('UNAUTHORIZED', 'Missing bearer token', {
        hint: 'Log in with `gridstator login` or set GRIDSTATOR_TOKEN.',
      });
    }
    const token = match[1]!;
    const kind = tokenKind(token);
    const row = kind ? this.store.tokenByHash(hashToken(token)) : undefined;
    const now = Date.now();
    if (!row || row.revoked_at !== null) {
      throw new GridstatorError('UNAUTHORIZED', 'Invalid or revoked token', {
        hint: 'Ask an admin for a new token (`gridstator token create`) and log in again.',
      });
    }
    if (row.expires_at !== null && row.expires_at < now) {
      throw new GridstatorError('UNAUTHORIZED', `Token "${row.name}" has expired`);
    }
    const principal: Principal = {
      tokenId: row.id,
      kind: row.kind,
      name: row.name,
      role: row.role,
      nodeId: row.node_id,
    };
    const ok =
      (requirement === 'client' && row.kind === 'client') ||
      (requirement === 'admin' && row.kind === 'client' && row.role === 'admin') ||
      (requirement === 'agent' && row.kind === 'agent') ||
      (requirement === 'client-or-agent' && (row.kind === 'client' || row.kind === 'agent')) ||
      (requirement === 'join' && row.kind === 'join');
    if (!ok) {
      if (requirement === 'admin' && row.kind === 'client') {
        throw new GridstatorError('FORBIDDEN', 'This action requires an admin token');
      }
      throw new GridstatorError(
        'FORBIDDEN',
        `A ${row.kind} token cannot be used for this endpoint`,
      );
    }
    if (row.kind === 'join' && (row.uses_remaining ?? 0) <= 0) {
      throw new GridstatorError('UNAUTHORIZED', `Join token "${row.name}" has already been used`, {
        hint: 'Create a new one with `gridstator token create --join`.',
      });
    }
    // Throttle last-used writes to once a minute per token.
    if ((this.lastTouched.get(row.id) ?? 0) < now - 60_000) {
      this.lastTouched.set(row.id, now);
      this.store.touchToken(row.id, now);
    }
    return principal;
  }
}

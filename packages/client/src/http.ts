import { Agent, request as httpsRequest, type RequestOptions } from 'node:https';
import type { IncomingMessage } from 'node:http';
import type { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { GridstatorError, errorFromBody, errorMessage } from '@gridstator/shared';

export const DEFAULT_PORT = 7300;

/** Accepts `host`, `host:port`, or `https://host:port[/]` and returns `https://host:port`. */
export function normalizeControllerUrl(input: string): string {
  let text = input.trim();
  if (/^http:\/\//i.test(text)) {
    throw new GridstatorError(
      'BAD_REQUEST',
      'Gridstator controllers only accept HTTPS connections',
      {
        hint: `Use https://${text.slice('http://'.length)}`,
      },
    );
  }
  if (!/^https:\/\//i.test(text)) text = `https://${text}`;
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new GridstatorError('BAD_REQUEST', `Invalid controller URL "${input}"`);
  }
  const port = url.port === '' ? DEFAULT_PORT : Number(url.port);
  const host =
    url.hostname.includes(':') && !url.hostname.startsWith('[')
      ? `[${url.hostname}]`
      : url.hostname;
  return `https://${host}:${port}`;
}

const TLS_ERROR_CODES = new Set([
  'UNABLE_TO_VERIFY_LEAF_SIGNATURE',
  'SELF_SIGNED_CERT_IN_CHAIN',
  'DEPTH_ZERO_SELF_SIGNED_CERT',
  'CERT_SIGNATURE_FAILURE',
  'UNABLE_TO_GET_ISSUER_CERT_LOCALLY',
  'UNABLE_TO_GET_ISSUER_CERT',
  'CERT_UNTRUSTED',
  'CERT_HAS_EXPIRED',
  'ERR_TLS_CERT_ALTNAME_INVALID',
]);

/** Converts low-level network errors into actionable GridstatorErrors. */
export function networkError(baseUrl: string, err: unknown): GridstatorError {
  if (err instanceof GridstatorError) return err;
  const code = (err as NodeJS.ErrnoException).code ?? '';
  if (TLS_ERROR_CODES.has(code)) {
    return new GridstatorError(
      'TLS_PIN_MISMATCH',
      `The controller at ${baseUrl} presented a certificate that is not signed by the pinned cluster CA (${code})`,
      {
        hint: 'If the controller was re-initialized, log in again with the new fingerprint. Otherwise something may be intercepting the connection.',
        cause: err,
      },
    );
  }
  if (
    [
      'ECONNREFUSED',
      'EHOSTUNREACH',
      'ENETUNREACH',
      'ENOTFOUND',
      'ETIMEDOUT',
      'ECONNRESET',
      'EAI_AGAIN',
    ].includes(code)
  ) {
    return new GridstatorError(
      'UNAVAILABLE',
      `Cannot reach the controller at ${baseUrl} (${code})`,
      {
        hint: 'Check that the controller is running (gridstator controller start) and reachable from this machine. Run `gridstator doctor` for details.',
        cause: err,
      },
    );
  }
  return new GridstatorError('UNAVAILABLE', `Request to ${baseUrl} failed: ${errorMessage(err)}`, {
    cause: err,
  });
}

export interface TlsTrust {
  /** PEM of the pinned cluster CA; it is the only trusted root. */
  ca: string;
}

/**
 * TLS options that trust only the pinned cluster CA. Hostname verification is skipped because the
 * private CA is the trust anchor and LAN addresses change; chain verification still applies.
 */
export function pinnedTlsOptions(trust: TlsTrust): {
  ca: string;
  checkServerIdentity: () => undefined;
  rejectUnauthorized: true;
} {
  return { ca: trust.ca, checkServerIdentity: () => undefined, rejectUnauthorized: true };
}

export function createPinnedAgent(trust: TlsTrust): Agent {
  return new Agent({ ...pinnedTlsOptions(trust), keepAlive: true, maxSockets: 32 });
}

export interface RawRequest {
  method: string;
  url: string;
  agent: Agent | false;
  headers?: Record<string, string>;
  body?: string | Buffer | Readable;
  timeoutMs?: number;
  rejectUnauthorized?: boolean;
}

/** Performs an HTTPS request and resolves with the response once headers arrive. */
export function rawRequest(req: RawRequest): Promise<IncomingMessage> {
  return new Promise((resolve, reject) => {
    const options: RequestOptions = {
      method: req.method,
      agent: req.agent,
      headers: req.headers,
      ...(req.rejectUnauthorized === false ? { rejectUnauthorized: false } : {}),
    };
    const r = httpsRequest(req.url, options, resolve);
    r.on('error', reject);
    if (req.timeoutMs) {
      r.setTimeout(req.timeoutMs, () =>
        r.destroy(Object.assign(new Error('request timed out'), { code: 'ETIMEDOUT' })),
      );
    }
    const body = req.body;
    if (body === undefined) r.end();
    else if (typeof body === 'string' || Buffer.isBuffer(body)) r.end(body);
    else pipeline(body, r).catch((err: unknown) => r.destroy(err as Error));
  });
}

export async function readBody(res: IncomingMessage, limit = 64 * 1024 * 1024): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of res) {
    size += (chunk as Buffer).length;
    if (size > limit) throw new GridstatorError('PAYLOAD_TOO_LARGE', 'Response too large');
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks);
}

export async function errorFromResponse(res: IncomingMessage): Promise<GridstatorError> {
  const text = (await readBody(res, 1024 * 1024).catch(() => Buffer.alloc(0))).toString('utf8');
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    body = undefined;
  }
  return errorFromBody(res.statusCode ?? 500, body);
}

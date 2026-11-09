/**
 * Error codes shared by the controller API, agents and clients. Each code maps to an HTTP status
 * (for API responses) and a CLI exit code, so errors keep their meaning across process boundaries.
 */
export const ERROR_CODES = {
  BAD_REQUEST: { status: 400, exitCode: 2 },
  UNAUTHORIZED: { status: 401, exitCode: 4 },
  FORBIDDEN: { status: 403, exitCode: 4 },
  NOT_FOUND: { status: 404, exitCode: 5 },
  CONFLICT: { status: 409, exitCode: 1 },
  PAYLOAD_TOO_LARGE: { status: 413, exitCode: 1 },
  UNAVAILABLE: { status: 503, exitCode: 3 },
  TLS_PIN_MISMATCH: { status: 495, exitCode: 4 },
  CONFIG: { status: 500, exitCode: 2 },
  INTERNAL: { status: 500, exitCode: 1 },
} as const;

export type ErrorCode = keyof typeof ERROR_CODES;

export interface ErrorBody {
  error: {
    code: ErrorCode;
    message: string;
    hint?: string | undefined;
    details?: unknown;
  };
}

export class GridstatorError extends Error {
  readonly code: ErrorCode;
  readonly hint: string | undefined;
  readonly details: unknown;

  constructor(
    code: ErrorCode,
    message: string,
    options: { hint?: string; details?: unknown; cause?: unknown } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = 'GridstatorError';
    this.code = code;
    this.hint = options.hint;
    this.details = options.details;
  }

  get status(): number {
    return ERROR_CODES[this.code].status;
  }

  get exitCode(): number {
    return ERROR_CODES[this.code].exitCode;
  }

  toBody(): ErrorBody {
    return {
      error: {
        code: this.code,
        message: this.message,
        ...(this.hint === undefined ? {} : { hint: this.hint }),
        ...(this.details === undefined ? {} : { details: this.details }),
      },
    };
  }
}

export function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && Object.hasOwn(ERROR_CODES, value);
}

/** Reconstructs a GridstatorError from an API error body (or falls back to INTERNAL). */
export function errorFromBody(status: number, body: unknown): GridstatorError {
  if (body && typeof body === 'object' && 'error' in body) {
    const err = body.error;
    if (err && typeof err === 'object') {
      const e = err as Record<string, unknown>;
      const code = isErrorCode(e['code']) ? e['code'] : statusToCode(status);
      const message = typeof e['message'] === 'string' ? e['message'] : `HTTP ${status}`;
      const hint = typeof e['hint'] === 'string' ? e['hint'] : undefined;
      return new GridstatorError(code, message, {
        ...(hint === undefined ? {} : { hint }),
        details: e['details'],
      });
    }
  }
  return new GridstatorError(statusToCode(status), `Controller responded with HTTP ${status}`);
}

function statusToCode(status: number): ErrorCode {
  switch (status) {
    case 400:
      return 'BAD_REQUEST';
    case 401:
      return 'UNAUTHORIZED';
    case 403:
      return 'FORBIDDEN';
    case 404:
      return 'NOT_FOUND';
    case 409:
      return 'CONFLICT';
    case 413:
      return 'PAYLOAD_TOO_LARGE';
    case 503:
      return 'UNAVAILABLE';
    default:
      return 'INTERNAL';
  }
}

export function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

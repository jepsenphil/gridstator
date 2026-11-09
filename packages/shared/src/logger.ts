import { pino, type Logger, type LoggerOptions } from 'pino';
import pretty from 'pino-pretty';

export type { Logger } from 'pino';

export type LogFormat = 'json' | 'pretty';

export interface CreateLoggerOptions {
  name: string;
  level?: string;
  /** Defaults to `pretty` when stderr is a TTY, otherwise `json`. */
  format?: LogFormat;
}

/** Structured logger writing to stderr (stdout is reserved for command output). */
export function createLogger(options: CreateLoggerOptions): Logger {
  const level = options.level ?? process.env['GRIDSTATOR_LOG_LEVEL'] ?? 'info';
  const format =
    options.format ??
    (process.env['GRIDSTATOR_LOG_FORMAT'] as LogFormat | undefined) ??
    (process.stderr.isTTY ? 'pretty' : 'json');
  const base: LoggerOptions = {
    name: options.name,
    level,
    redact: {
      paths: [
        'req.headers.authorization',
        'headers.authorization',
        'token',
        'credential',
        '*.token',
        '*.credential',
      ],
      censor: '[redacted]',
    },
  };
  if (format === 'pretty') {
    return pino(
      base,
      pretty({ destination: 2, colorize: true, translateTime: 'HH:MM:ss', ignore: 'pid,hostname' }),
    );
  }
  return pino(base, pino.destination(2));
}

/** A logger that discards everything (tests, library defaults). */
export function silentLogger(): Logger {
  return pino({ level: 'silent' });
}

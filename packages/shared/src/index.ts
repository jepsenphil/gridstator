export * from './errors.js';
export * from './ids.js';
export * from './format.js';
export * from './fs.js';
export * from './paths.js';
export * from './logger.js';
export * from './tls.js';
export * from './install.js';
export * from './machine.js';

export const GRIDSTATOR_VERSION = '0.1.0';

/** Decodes a `ws` message payload (Buffer, ArrayBuffer or fragments) as UTF-8 text. */
export function decodeWsData(data: Buffer | ArrayBuffer | Buffer[]): string {
  if (Buffer.isBuffer(data)) return data.toString('utf8');
  if (Array.isArray(data)) return Buffer.concat(data).toString('utf8');
  return Buffer.from(data).toString('utf8');
}

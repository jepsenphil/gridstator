import { z } from 'zod';

export const DigestSchema = z
  .string()
  .regex(/^sha256:[0-9a-f]{64}$/, 'expected sha256:<64 lowercase hex chars>');
export type Digest = z.infer<typeof DigestSchema>;

export const SnapshotIdSchema = DigestSchema;

export const IdSchema = z.string().regex(/^[0-9A-Z]{26}$/, 'expected a 26-character id');

/** Labels are `key` or `key=value`. */
export const LabelSchema = z
  .string()
  .min(1)
  .max(128)
  .regex(
    /^[A-Za-z0-9][A-Za-z0-9._-]*(=[A-Za-z0-9._:/-]+)?$/,
    'labels look like "fast", "gpu" or "os=linux"',
  );

export const EnvNameSchema = z
  .string()
  .regex(
    /^[A-Za-z_][A-Za-z0-9_]*$/,
    'environment variable names must match [A-Za-z_][A-Za-z0-9_]*',
  );

export const EnvMapSchema = z.record(EnvNameSchema, z.string().max(128 * 1024));

/** Epoch milliseconds. */
export const TimestampSchema = z.number().int().nonnegative();

import { z } from 'zod';
import { TimestampSchema } from './common.js';

/** Only published releases with a Gridstator package and published checksums are offered. */
export const ReleaseViewSchema = z.object({
  tag: z.string().min(1).max(128),
  version: z.string().min(1).max(128),
  name: z.string().max(512),
  url: z.string(),
  publishedAt: TimestampSchema,
  prerelease: z.boolean(),
  notes: z.string(),
  packageName: z.string().max(255),
});
export type ReleaseView = z.infer<typeof ReleaseViewSchema>;

export const ListReleasesResponseSchema = z.object({ releases: z.array(ReleaseViewSchema) });

import { z } from 'zod';
import { EnvMapSchema, LabelSchema } from './common.js';
import { RetryPolicySchema } from './job.js';

export const PROJECT_CONFIG_FILENAMES = ['gridstator.yml', 'gridstator.yaml'] as const;

/** Sizes may be numbers (MiB) or strings such as "4g". */
const SizeSchema = z.union([z.number().int().positive(), z.string().min(1)]);

const ResourcesConfigSchema = z
  .object({
    cpus: z.number().positive().optional(),
    memory: SizeSchema.optional(),
  })
  .strict();

const TaskConfigSchema = z
  .object({
    description: z.string().optional(),
    /** A shell string (run with `sh -c`) or an argv array. */
    command: z.union([z.string().min(1), z.array(z.string()).min(1)]),
    image: z.string().optional(),
    env: EnvMapSchema.optional(),
    /** Names of client environment variables forwarded as secrets. */
    secretEnv: z.array(z.string()).optional(),
    workdir: z.string().optional(),
    resources: ResourcesConfigSchema.optional(),
    labels: z.array(LabelSchema).optional(),
    node: z.string().optional(),
    timeout: z.string().optional(),
    priority: z.number().int().min(-100).max(100).optional(),
    retry: RetryPolicySchema.partial().optional(),
  })
  .strict();
export type TaskConfig = z.infer<typeof TaskConfigSchema>;

export const ProjectConfigSchema = z
  .object({
    version: z.literal(1).default(1),
    /** Cache-affinity key; defaults to the project directory name. */
    project: z.string().optional(),
    defaults: z
      .object({
        image: z.string().optional(),
        resources: ResourcesConfigSchema.optional(),
        labels: z.array(LabelSchema).optional(),
        timeout: z.string().optional(),
      })
      .strict()
      .default({}),
    snapshot: z
      .object({
        /** Extra globs to include even if git ignores them. */
        include: z.array(z.string()).default([]),
        /** Extra globs to exclude. */
        exclude: z.array(z.string()).default([]),
        /** Globs of secret-pattern files that may be included anyway. */
        allowSecrets: z.array(z.string()).default([]),
        /** Files larger than this are excluded with a warning. */
        maxFileSize: SizeSchema.default('256m'),
      })
      .strict()
      .default({ include: [], exclude: [], allowSecrets: [], maxFileSize: '256m' }),
    tasks: z
      .record(z.string().regex(/^[A-Za-z0-9][A-Za-z0-9:._-]*$/), TaskConfigSchema)
      .default({}),
  })
  .strict();
export type ProjectConfig = z.infer<typeof ProjectConfigSchema>;

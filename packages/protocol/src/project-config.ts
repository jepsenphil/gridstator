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
    /** Files/directories (relative to the project root) collected as artifacts. */
    artifacts: z.array(z.string()).optional(),
    /** JUnit XML report written by the task (relative to the project root); shown in VS Code's Testing view. */
    junit: z.string().optional(),
    /** Split the task into this many shards (use {shard} and {total} in the command). */
    shards: z.number().int().min(1).max(64).optional(),
    /** Preferred workers (soft): used when eligible, otherwise any node. */
    prefer: z.array(z.string()).optional(),
  })
  .strict();
export type TaskConfig = z.infer<typeof TaskConfigSchema>;

const BuildConfigSchema = z
  .object({
    description: z.string().optional(),
    /** Dockerfile path relative to the project root (default: <context>/Dockerfile). */
    file: z.string().optional(),
    /** Build context relative to the project root (default "."). */
    context: z.string().optional(),
    /** Dockerfile stage (`--target`). */
    stage: z.string().optional(),
    buildArgs: z.record(z.string(), z.string()).optional(),
    tags: z.array(z.string()).optional(),
    platforms: z.array(z.string()).optional(),
    output: z.enum(['load', 'push', 'tar', 'none']).optional(),
    secretEnv: z.array(z.string()).optional(),
    labels: z.array(LabelSchema).optional(),
    node: z.string().optional(),
  })
  .strict();
export type BuildConfig = z.infer<typeof BuildConfigSchema>;

const WorkflowLauncherConfigSchema = z
  .record(
    z.string().regex(/^(?:\.github\/workflows\/)?[A-Za-z0-9_-][A-Za-z0-9_.-]*\.ya?ml$/),
    z
      .object({
        description: z.string().optional(),
        /** Only these jobs may be selected directly; their dependencies still run. */
        jobs: z
          .array(z.string().refine((job) => job.trim().length > 0, 'Expected a nonempty job ID'))
          .min(1)
          .refine((jobs) => new Set(jobs).size === jobs.length, 'Duplicate job IDs')
          .optional(),
      })
      .strict(),
  )
  .superRefine((workflows, context) => {
    const seen = new Set<string>();
    for (const file of Object.keys(workflows)) {
      const name = file.replace(/^\.github\/workflows\//, '');
      if (seen.has(name)) {
        context.addIssue({ code: 'custom', path: [file], message: 'Workflow is listed twice' });
      }
      seen.add(name);
    }
  });

const ActionsConfigSchema = z
  .object({
    /** runs-on label → image, e.g. { "ubuntu-latest": "catthehacker/ubuntu:act-latest" }. */
    platforms: z.record(z.string(), z.string()).optional(),
    /** Mount the worker's Docker socket into workflow containers. */
    dockerSocket: z.boolean().optional(),
    /** Client environment variables forwarded as workflow secrets (e.g. GITHUB_TOKEN). */
    secretEnv: z.array(z.string()).optional(),
    vars: z.record(z.string(), z.string()).optional(),
    /** Optional allowlist for the VS Code launcher; omitted discovers all, {} hides all. */
    launcher: WorkflowLauncherConfigSchema.optional(),
  })
  .strict();
export type ActionsConfig = z.infer<typeof ActionsConfigSchema>;

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
        /** Preferred workers (soft) for every job from this project. */
        prefer: z.array(z.string()).optional(),
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
    builds: z
      .record(z.string().regex(/^[A-Za-z0-9][A-Za-z0-9:._-]*$/), BuildConfigSchema)
      .default({}),
    actions: ActionsConfigSchema.default({}),
  })
  .strict();
export type ProjectConfig = z.infer<typeof ProjectConfigSchema>;

import { execFile } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { promisify } from 'node:util';
import type { Command } from 'commander';
import { GridstatorError, formatBytes, parseDuration, parseMegabytes } from '@gridstator/shared';
import {
  DEFAULT_RESOURCES,
  isTerminal,
  type JobSpecInput,
  type ProjectConfig,
} from '@gridstator/protocol';
import { discoverWorkflows, findWorkflow, type WorkflowInfo } from '@gridstator/discovery';
import type { GridstatorClient } from '@gridstator/client';
import { findProject, loadProjectConfig } from '@gridstator/snapshot';
import { c, exitCodeForJob, info, printJson, table } from '../output.js';
import { parseLabels } from '../settings-flags.js';
import {
  captureProject,
  collect,
  exitState,
  followJob,
  normalizeRel,
  parseEnvPairs,
  secretEnvFrom,
  summarize,
  uploadCaptured,
  withClient,
} from './client.js';

const execFileAsync = promisify(execFile);

interface PlacementFlags {
  node?: string;
  prefer: string[];
  label: string[];
  cpus?: string;
  memory?: string;
  timeout?: string;
  priority?: string;
  name?: string;
  secretEnv: string[];
  detach?: boolean;
  json?: boolean;
}

function addPlacementOptions(cmd: Command): Command {
  return cmd
    .option('--node <node>', 'run on this node (name or id)')
    .option(
      '--prefer <node>',
      'prefer this worker when it can take the job (repeatable)',
      collect,
      [],
    )
    .option('-l, --label <label>', 'require a node label (repeatable)', collect, [])
    .option('--cpus <n>', 'CPUs to reserve')
    .option('--memory <size>', 'memory to reserve, e.g. 8g')
    .option('--timeout <duration>', 'stop the job after this long, e.g. 30m')
    .option('--priority <n>', 'higher runs first (-100..100)')
    .option('--name <name>', 'display name for the job')
    .option(
      '-s, --secret-env <NAME>',
      'forward a secret from this shell; masked in logs (repeatable)',
      collect,
      [],
    )
    .option('-d, --detach', 'print the job id and exit instead of streaming logs')
    .option('--json', 'print the submitted job as JSON (implies --detach)');
}

function placement(
  flags: PlacementFlags,
  config: ProjectConfig,
  extraLabels: string[] = [],
  node?: string,
) {
  const resources = config.defaults.resources ?? {};
  const memory = flags.memory ?? resources.memory;
  const timeout = flags.timeout ?? config.defaults.timeout;
  const labels = [
    ...(config.defaults.labels ?? []),
    ...extraLabels,
    ...flags.label.flatMap(parseLabels),
  ];
  const target = flags.node ?? node;
  return {
    ...(flags.name ? { name: flags.name } : {}),
    resources: {
      cpus:
        flags.cpus !== undefined ? Number(flags.cpus) : (resources.cpus ?? DEFAULT_RESOURCES.cpus),
      memoryMb:
        memory === undefined
          ? DEFAULT_RESOURCES.memoryMb
          : typeof memory === 'number'
            ? memory
            : parseMegabytes(memory),
    },
    target: {
      labels: [...new Set(labels)],
      ...(target ? { node: target } : {}),
      prefer: [...new Set([...(config.defaults.prefer ?? []), ...flags.prefer])],
    },
    ...(timeout ? { timeoutSeconds: Math.ceil(parseDuration(timeout) / 1000) } : {}),
    priority: flags.priority !== undefined ? Number(flags.priority) : 0,
  };
}

/** Captures and uploads the project, submits the job, then follows it (unless detached). */
async function submit(
  client: GridstatorClient,
  build: (ctx: {
    snapshot: string;
    workdir: string;
    root: string;
    config: ProjectConfig;
  }) => JobSpecInput | Promise<JobSpecInput>,
  flags: PlacementFlags,
): Promise<void> {
  const quiet = flags.json ?? false;
  const cap = await captureProject(process.cwd(), { quiet });
  await uploadCaptured(client, cap.captured, quiet);
  const spec = await build({
    snapshot: cap.captured.id,
    workdir: cap.workdir,
    root: cap.root,
    config: cap.config,
  });
  const job = await client.submitJob({
    ...spec,
    project: cap.config.project ?? basename(cap.root),
  });
  if (flags.json) return printJson(job);
  if (flags.detach) {
    process.stdout.write(job.id + '\n');
    return;
  }
  info(
    c.dim(
      `Job ${job.id} submitted (snapshot ${cap.captured.id.slice(7, 19)}); ${job.stateReason ?? job.state}`,
      process.stderr,
    ),
  );
  const final = await followJob(client, job, { cancelOnInterrupt: true });
  if (isTerminal(final.state)) {
    summarize(final);
    const image = final.outputs?.image;
    if (image?.id && final.state === 'succeeded') {
      info(
        `Image ${image.id}${image.tags.length ? ` (${image.tags.join(', ')})` : ''}${image.sizeBytes !== null ? `, ${formatBytes(image.sizeBytes)}` : ''} on ${final.nodeName}`,
      );
    }
    for (const a of final.artifacts)
      info(
        c.dim(
          `Artifact ${a.name} (${formatBytes(a.size)}): gridstator artifacts ${final.id} --download ${a.name}`,
          process.stderr,
        ),
      );
    exitState.code = exitCodeForJob(final);
  }
}

async function gitContext(
  root: string,
): Promise<{ branch?: string; remoteUrl?: string; headSha?: string }> {
  const run = async (args: string[]) => {
    try {
      return (await execFileAsync('git', ['-C', root, ...args])).stdout.trim() || undefined;
    } catch {
      return undefined;
    }
  };
  const [branch, remoteUrl, headSha] = await Promise.all([
    run(['rev-parse', '--abbrev-ref', 'HEAD']),
    run(['remote', 'get-url', 'origin']),
    run(['rev-parse', 'HEAD']),
  ]);
  return {
    ...(branch && branch !== 'HEAD' ? { branch } : {}),
    ...(remoteUrl ? { remoteUrl } : {}),
    ...(headSha ? { headSha } : {}),
  };
}

function parseKeyValues(pairs: string[], sepChar: '=' | ':', what: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const p of pairs) {
    const i = p.indexOf(sepChar);
    if (i <= 0)
      throw new GridstatorError('BAD_REQUEST', `${what} "${p}" must look like key${sepChar}value`);
    out[p.slice(0, i)] = p.slice(i + 1);
  }
  return out;
}

function printWorkflows(workflows: WorkflowInfo[]): void {
  for (const w of workflows) {
    process.stdout.write(`${c.bold(w.name)}  ${c.dim(w.file)}\n`);
    if (w.parseError) {
      process.stdout.write(`  ${c.red(`cannot parse: ${w.parseError}`)}\n\n`);
      continue;
    }
    process.stdout.write(
      `  events: ${w.events.join(', ') || '-'}${w.dispatchInputs.length ? `   inputs: ${w.dispatchInputs.map((i) => `${i.name}${i.required ? '*' : ''}`).join(', ')}` : ''}\n`,
    );
    for (const warn of w.warnings) process.stdout.write(`  ${c.yellow('!')} ${warn}\n`);
    const rows = w.jobs.map((j) => [
      j.id,
      j.name === j.id ? '' : j.name,
      j.usesWorkflow ? `uses ${j.usesWorkflow}` : j.runsOn.join(', '),
      j.needs.join(', ') || '-',
      j.runnable ? (j.warnings.length ? c.yellow('with caveats') : c.green('yes')) : c.red('no'),
    ]);
    if (rows.length)
      process.stdout.write(
        table(
          ['  JOB', 'NAME', 'RUNS ON', 'NEEDS', 'RUNNABLE'],
          rows.map((r) => [`  ${r[0]}`, ...r.slice(1)]),
        ) + '\n',
      );
    for (const j of w.jobs)
      for (const warn of j.warnings) process.stdout.write(`  ${c.yellow('!')} ${j.id}: ${warn}\n`);
    process.stdout.write('\n');
  }
}

export function registerWorkloadCommands(program: Command): void {
  // ---- docker builds -------------------------------------------------------------------------

  addPlacementOptions(
    program
      .command('build')
      .description('Build a Docker image from this project on a cluster node (BuildKit)')
      .argument('[context]', 'build context directory (default: .)')
      .option('-f, --file <Dockerfile>', 'Dockerfile (default: <context>/Dockerfile)')
      .option('-b, --build <name>', 'use a build defined under `builds:` in gridstator.yml')
      .option('--target <stage>', 'Dockerfile stage to build')
      .option('--build-arg <KEY=value>', 'build argument (repeatable)', collect, [])
      .option('-t, --tag <tag>', 'image tag (repeatable)', collect, [])
      .option(
        '--platform <platform>',
        'target platform, e.g. linux/arm64 (repeatable)',
        collect,
        [],
      )
      .option('--output <mode>', 'load (into the worker), push, tar (download as artifact) or none')
      .option('--no-cache', 'do not use the build cache')
      .option('--pull', 'always pull newer base images')
      .option(
        '--cache-from <spec>',
        'external cache source, e.g. type=registry,ref=... (repeatable)',
        collect,
        [],
      )
      .option('--cache-to <spec>', 'external cache destination (repeatable)', collect, []),
  ).action(
    async (
      contextArg: string | undefined,
      flags: PlacementFlags & {
        file?: string;
        build?: string;
        target?: string;
        buildArg: string[];
        tag: string[];
        platform: string[];
        output?: string;
        cache: boolean;
        pull?: boolean;
        cacheFrom: string[];
        cacheTo: string[];
      },
    ) =>
      withClient((client) =>
        submit(
          client,
          ({ snapshot, workdir, config }) => {
            let def = flags.build ? config.builds[flags.build] : undefined;
            if (flags.build && !def) {
              const known = Object.keys(config.builds);
              throw new GridstatorError(
                'BAD_REQUEST',
                `No build named "${flags.build}" in gridstator.yml`,
                {
                  hint: known.length
                    ? `Known builds: ${known.join(', ')}`
                    : 'Define builds under `builds:` in gridstator.yml.',
                },
              );
            }
            def ??= {};
            // Paths from gridstator.yml are relative to the project root; CLI paths to the current directory.
            const context =
              contextArg !== undefined
                ? normalizeRel(workdir, contextArg)
                : def.context
                  ? normalizeRel('.', def.context)
                  : workdir;
            const dockerfile =
              flags.file !== undefined
                ? normalizeRel(workdir, flags.file)
                : def.file
                  ? normalizeRel('.', def.file)
                  : undefined;
            const output = flags.output ?? def.output ?? 'load';
            if (!['load', 'push', 'tar', 'none'].includes(output))
              throw new GridstatorError('BAD_REQUEST', '--output must be load, push, tar or none');
            const stage = flags.target ?? def.stage;
            return {
              kind: 'docker-build',
              ...placement(flags, config, def.labels, def.node),
              ...(flags.name
                ? { name: flags.name }
                : flags.build
                  ? { name: `build:${flags.build}` }
                  : {}),
              snapshot,
              context,
              ...(dockerfile ? { dockerfile } : {}),
              ...(stage ? { stage } : {}),
              buildArgs: { ...def.buildArgs, ...parseKeyValues(flags.buildArg, '=', 'build arg') },
              tags: flags.tag.length ? flags.tag : (def.tags ?? []),
              platforms: flags.platform.length ? flags.platform : (def.platforms ?? []),
              noCache: !flags.cache,
              pull: flags.pull ?? false,
              output: output as 'load' | 'push' | 'tar' | 'none',
              cacheFrom: flags.cacheFrom,
              cacheTo: flags.cacheTo,
              secretEnv: secretEnvFrom([...(def.secretEnv ?? []), ...flags.secretEnv]),
            };
          },
          flags,
        ),
      ),
  );

  // ---- GitHub Actions ------------------------------------------------------------------------

  const actions = program
    .command('actions')
    .description('Run GitHub Actions workflows on the cluster with act');

  actions
    .command('list')
    .description('List workflows and jobs in this project, with local-execution caveats')
    .option('--json', 'machine-readable output')
    .action(async (opts: { json?: boolean }) => {
      const loc = await findProject(process.cwd());
      const workflows = await discoverWorkflows(loc.root);
      if (opts.json) return printJson(workflows);
      if (workflows.length === 0)
        return info(`No workflows found in ${loc.root}/.github/workflows`);
      printWorkflows(workflows);
      info(c.dim('Run one with: gridstator actions run <workflow> [--job <id>]', process.stderr));
    });

  addPlacementOptions(
    actions
      .command('run')
      .description('Run a workflow (or one job) against this working tree on a cluster node')
      .argument('<workflow>', 'workflow file name (ci.yml), path, or name')
      .option('-j, --job <id>', 'run only this job')
      .option(
        '-e, --event <name>',
        'triggering event (default: push, else the first event the workflow declares)',
      )
      .option('-i, --input <key=value>', 'workflow_dispatch input (repeatable)', collect, [])
      .option('--var <key=value>', 'value for the vars context (repeatable)', collect, [])
      .option(
        '--env <KEY=value>',
        'environment variable for workflow steps (repeatable)',
        collect,
        [],
      )
      .option(
        '-P, --platform <label=image>',
        'runner image for a runs-on label (repeatable)',
        collect,
        [],
      )
      .option(
        '--matrix <key:value>',
        'run only matching matrix combinations (repeatable)',
        collect,
        [],
      )
      .option('--event-payload <file>', 'JSON file used as github.event')
      .option('--docker-socket', "mount the worker's Docker socket into workflow containers"),
  ).action(
    async (
      workflowArg: string,
      flags: PlacementFlags & {
        job?: string;
        event?: string;
        input: string[];
        var: string[];
        env: string[];
        platform: string[];
        matrix: string[];
        eventPayload?: string;
        dockerSocket?: boolean;
      },
    ) => {
      const loc = await findProject(process.cwd());
      const config = await loadProjectConfig(loc.configPath);
      const workflows = await discoverWorkflows(loc.root);
      const wf = findWorkflow(workflows, workflowArg);
      if (!wf) {
        throw new GridstatorError(
          'NOT_FOUND',
          `No workflow "${workflowArg}" in ${loc.root}/.github/workflows`,
          {
            hint: workflows.length
              ? `Available: ${workflows.map((w) => w.file.split('/').pop()).join(', ')}`
              : undefined,
          },
        );
      }
      if (wf.parseError)
        throw new GridstatorError('BAD_REQUEST', `Cannot parse ${wf.file}: ${wf.parseError}`);
      const jobs = flags.job ? wf.jobs.filter((j) => j.id === flags.job) : wf.jobs;
      if (flags.job && jobs.length === 0) {
        throw new GridstatorError('NOT_FOUND', `Workflow ${wf.file} has no job "${flags.job}"`, {
          hint: `Jobs: ${wf.jobs.map((j) => j.id).join(', ')}`,
        });
      }
      if (jobs.length > 0 && jobs.every((j) => !j.runnable)) {
        throw new GridstatorError(
          'BAD_REQUEST',
          `None of the selected jobs can run locally: ${jobs.flatMap((j) => j.warnings).join('; ')}`,
        );
      }
      if (!flags.json) {
        for (const w of [
          ...wf.warnings,
          ...jobs.flatMap((j) => j.warnings.map((x) => `${j.id}: ${x}`)),
        ])
          info(c.yellow(`! ${w}`, process.stderr));
      }
      const event = flags.event ?? wf.defaultEvent;
      const inputs = parseKeyValues(flags.input, '=', 'input');
      const missing =
        event === 'workflow_dispatch'
          ? wf.dispatchInputs.filter(
              (i) => i.required && i.default === undefined && !(i.name in inputs),
            )
          : [];
      if (missing.length)
        throw new GridstatorError(
          'BAD_REQUEST',
          `Missing required input(s): ${missing.map((i) => i.name).join(', ')}`,
          { hint: 'Pass them with --input name=value.' },
        );
      const eventPayload = flags.eventPayload
        ? (JSON.parse(await readFile(flags.eventPayload, 'utf8')) as Record<string, unknown>)
        : undefined;
      const git = await gitContext(loc.root);
      await withClient((client) =>
        submit(
          client,
          ({ snapshot }) => ({
            kind: 'actions',
            ...placement(flags, config),
            snapshot,
            workflow: wf.file,
            ...(flags.job ? { job: flags.job } : {}),
            event,
            ...(eventPayload ? { eventPayload } : {}),
            inputs,
            vars: { ...config.actions.vars, ...parseKeyValues(flags.var, '=', 'var') },
            env: parseEnvPairs(flags.env),
            platforms: {
              ...config.actions.platforms,
              ...parseKeyValues(flags.platform, '=', 'platform'),
            },
            matrix: flags.matrix,
            dockerSocket: flags.dockerSocket ?? config.actions.dockerSocket ?? false,
            git,
            secretEnv: secretEnvFrom([...(config.actions.secretEnv ?? []), ...flags.secretEnv]),
          }),
          flags,
        ),
      );
    },
  );

  // ---- artifacts -----------------------------------------------------------------------------

  program
    .command('artifacts')
    .description('List or download the artifacts of a job')
    .argument('<id>', 'job id')
    .option('--download <name>', 'download this artifact')
    .option(
      '-o, --out <path>',
      'where to write the download ("-" for stdout; default: the artifact file name)',
    )
    .option('--json', 'machine-readable output')
    .action(async (id: string, opts: { download?: string; out?: string; json?: boolean }) =>
      withClient(async (client) => {
        if (!opts.download) {
          const artifacts = await client.listArtifacts(id);
          if (opts.json) return printJson(artifacts);
          if (artifacts.length === 0) return info(`Job ${id} has no artifacts.`);
          process.stdout.write(
            table(
              ['NAME', 'SIZE', 'DIGEST'],
              artifacts.map((a) => [a.name, formatBytes(a.size), a.digest.slice(0, 19)]),
            ) + '\n',
          );
          return;
        }
        const res = await client.downloadArtifact(id, opts.download);
        const out = opts.out ?? opts.download.split('/').pop()!;
        if (out === '-') {
          await pipeline(res, process.stdout);
          return;
        }
        await pipeline(res, createWriteStream(out, { flags: 'wx' }));
        info(`Saved ${opts.download} to ${out}`);
      }),
    );
}

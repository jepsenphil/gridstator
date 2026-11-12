import type { WorkflowInfo } from '@gridstator/discovery';
import type { ActionsConfig } from '@gridstator/protocol';
import { GridstatorError } from '@gridstator/shared';
import type { LaunchItem, LaunchOptions } from './messages';

const filename = (file: string) => file.replace(/^\.github\/workflows\//, '');

function entryFor(actions: ActionsConfig, file: string) {
  return actions.launcher?.[file] ?? actions.launcher?.[filename(file)];
}

/** Curate launch choices only; reusable dependencies stay in the snapshot. */
export function launcherWorkflows(workflows: WorkflowInfo[], actions: ActionsConfig) {
  if (actions.launcher === undefined)
    return workflows.map((workflow) => ({ workflow, description: undefined, wholeWorkflow: true }));

  return Object.entries(actions.launcher).map(([file, entry]) => {
    const workflow = workflows.find((candidate) => filename(candidate.file) === filename(file));
    if (!workflow)
      throw new GridstatorError('BAD_REQUEST', `actions.launcher: workflow ${file} does not exist`);
    for (const id of entry.jobs ?? []) {
      if (!workflow.jobs.some((job) => job.id === id))
        throw new GridstatorError(
          'BAD_REQUEST',
          `actions.launcher: job ${file}:${id} does not exist`,
        );
    }
    return {
      workflow: {
        ...workflow,
        jobs: entry.jobs
          ? workflow.jobs.filter((job) => entry.jobs!.includes(job.id))
          : workflow.jobs,
      },
      description: entry.description,
      wholeWorkflow: entry.jobs === undefined,
    };
  });
}

/** Enforce the same choices at submission, including stale selections and reruns. */
export function assertWorkflowLaunch(
  item: LaunchItem,
  options: LaunchOptions,
  actions: ActionsConfig,
): void {
  if (item.kind !== 'workflow' || !item.workflow || actions.launcher === undefined) return;
  const file = item.workflow.file;
  const entry = entryFor(actions, file);
  if (!entry)
    throw new GridstatorError(
      'BAD_REQUEST',
      `${file} is not available in the local launcher. Choose a workflow from Run.`,
    );
  if (entry.jobs) {
    if (!options.job)
      throw new GridstatorError(
        'BAD_REQUEST',
        'Choose one of the available jobs; this workflow contains internal jobs.',
      );
    if (!entry.jobs.includes(options.job))
      throw new GridstatorError(
        'BAD_REQUEST',
        `${file}:${options.job} is not available in the local launcher. Choose an available job.`,
      );
  }
}

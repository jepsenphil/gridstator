import type { Command } from 'commander';
import { GridstatorError, parseDuration } from '@gridstator/shared';
import {
  AutoUpdateModeSchema,
  NodeAutoUpdateSchema,
  type Node,
  type NodeUpdateState,
  type ClusterSettings,
  type Rollout,
  type ToolOverride,
  type UpdatesView,
} from '@gridstator/protocol';
import type { GridstatorClient } from '@gridstator/client';
import { ago, c, info, notCurrent, printJson, summarizeVersions, table } from '../output.js';
import { exitState, withClient } from './client.js';

/** `agents status --check` exits with this when a reachable worker is not on the controller's build. */
export const NOT_CURRENT_EXIT_CODE = 3;

function stateText(state: NodeUpdateState | undefined): string {
  switch (state) {
    case 'current':
      return c.green('current');
    case 'behind':
      return c.yellow('behind');
    case 'updating':
      return c.cyan('updating');
    case 'failed':
      return c.red('failed');
    case 'unknown':
      return c.gray('unknown');
    default:
      return c.gray('-');
  }
}

function rolloutLines(r: Rollout): string[] {
  const done = r.nodes.filter((n) => n.state === 'done').length;
  const head =
    r.state === 'running'
      ? c.cyan(`Rollout to ${r.target.id} running`)
      : r.state === 'halted'
        ? c.red(`Rollout to ${r.target.id} halted: ${r.haltReason ?? 'unknown'}`)
        : c.green(`Rollout to ${r.target.id} finished`);
  const lines = [
    `${head}  (${done}/${r.nodes.length} done, started by ${r.startedBy} ${ago(r.startedAt)}${r.now ? ', --now' : ''})`,
  ];
  for (const n of r.nodes) {
    lines.push(
      `  ${n.name.padEnd(20)} ${n.state.padEnd(9)}${n.auto ? ' (auto)' : ''}${n.detail ? `  ${c.dim(n.detail)}` : ''}`,
    );
  }
  if (r.state === 'halted')
    lines.push(c.dim('  Fix the cause, then run `gridstator agents update` to try again.'));
  return lines;
}

function printStatus(view: UpdatesView, nodes: Node[]): void {
  const ctl = view.controller;
  const window = view.updateWindow ? ` (${view.updateWindow.start}-${view.updateWindow.end})` : '';
  process.stdout.write(
    `${c.bold('Controller build')} ${ctl.id}${ctl.updatable ? '' : c.yellow(`  (workers cannot update: ${ctl.reason ?? 'unknown'})`)}\n`,
  );
  process.stdout.write(`${c.bold('Auto-update')} ${view.autoUpdate}${window}\n`);
  for (const t of view.toolchain ?? []) {
    const where = t.platforms
      .map(
        (p) =>
          `${p.platform} ${p.state === 'ready' ? 'ready' : p.state}${p.reason ? ` (${p.reason})` : ''}`,
      )
      .join(', ');
    process.stdout.write(
      `${c.bold(t.name === 'node' ? 'Node.js' : 'act')} ${t.version ?? 'not managed'}${t.source === 'override' ? ' (cluster setting)' : ''}${where ? `  ${c.dim(where)}` : ''}\n`,
    );
  }
  process.stdout.write('\n');
  if (nodes.length > 0) {
    const rows = nodes.map((n) => [
      n.name,
      n.status === 'online' ? c.green('online') : c.gray('offline'),
      n.update?.build ?? n.info?.build?.id ?? (n.info ? `v${n.info.agentVersion}` : '-'),
      stateText(n.update?.state),
      n.update
        ? `${n.update.autoUpdate}${n.settings.autoUpdate === 'inherit' ? '' : ' (node)'}`
        : '-',
      n.update?.detail ?? '',
    ]);
    process.stdout.write(
      table(['NODE', 'STATUS', 'BUILD', 'UPDATE', 'AUTO-UPDATE', 'DETAIL'], rows) + '\n\n',
    );
  }
  process.stdout.write(`${summarizeVersions(nodes, ctl.id)}\n`);
  if (view.rollout) process.stdout.write('\n' + rolloutLines(view.rollout).join('\n') + '\n');
}

async function fetchStatus(
  client: GridstatorClient,
): Promise<{ view: UpdatesView; nodes: Node[] }> {
  const [view, nodes] = await Promise.all([client.updates(), client.listNodes()]);
  return { view, nodes };
}

async function follow(client: GridstatorClient): Promise<Rollout | null> {
  let last = '';
  for (;;) {
    const view = await client.updates();
    const r = view.rollout;
    const text = r ? rolloutLines(r).join('\n') : 'No rollout.';
    if (text !== last) {
      process.stdout.write(text + '\n\n');
      last = text;
    }
    if (!r || r.state !== 'running') return r;
    await new Promise((res) => setTimeout(res, 2000));
  }
}

function parseWindow(text: string): { start: string; end: string } {
  const m = /^(\d{2}:\d{2})-(\d{2}:\d{2})$/.exec(text.trim());
  if (!m)
    throw new GridstatorError('BAD_REQUEST', `Invalid window "${text}"`, {
      hint: 'Use HH:MM-HH:MM in the controller’s local time, e.g. 02:00-05:00.',
    });
  return { start: m[1]!, end: m[2]! };
}

export function registerAgentsCommands(program: Command): void {
  const agents = program
    .command('agents')
    .description('Keep worker agents on the controller’s build (status, updates, auto-update)');

  agents
    .command('status')
    .description('Show which workers run the controller’s build')
    .option('--json', 'machine-readable output')
    .option(
      '--check',
      `exit with code ${NOT_CURRENT_EXIT_CODE} if a reachable worker is not on the controller’s build (offline workers do not count)`,
    )
    .action(async (opts: { json?: boolean; check?: boolean }) =>
      withClient(async (client) => {
        const { view, nodes } = await fetchStatus(client);
        const bad = notCurrent(nodes);
        if (opts.json) {
          printJson({
            controller: view.controller,
            autoUpdate: view.autoUpdate,
            updateWindow: view.updateWindow,
            current: bad.length === 0,
            nodes: nodes.map((n) => ({
              id: n.id,
              name: n.name,
              status: n.status,
              lastSeenAt: n.lastSeenAt,
              ...(n.update ?? { state: 'unknown', build: null, detail: 'controller too old' }),
            })),
            rollout: view.rollout,
          });
        } else {
          printStatus(view, nodes);
        }
        if (opts.check && bad.length > 0) exitState.code = NOT_CURRENT_EXIT_CODE;
      }),
    );

  agents
    .command('update')
    .description(
      'Update workers to the controller’s build, one at a time: drain, update, wait until it is back (admin)',
    )
    .argument('[nodes...]', 'node names or ids (default: every online worker that is not current)')
    .option('--now', 'do not wait for running jobs: cancel them and update straight away')
    .option('--drain-timeout <duration>', 'cancel jobs still running after this long', '1h')
    .option('--wait', 'follow the rollout until it finishes or halts')
    .action(
      async (nodes: string[], opts: { now?: boolean; drainTimeout: string; wait?: boolean }) =>
        withClient(async (client) => {
          const view = await client.startUpdate({
            ...(nodes.length > 0 ? { nodes } : {}),
            now: opts.now ?? false,
            drainTimeoutSeconds: Math.max(1, Math.round(parseDuration(opts.drainTimeout) / 1000)),
          });
          const r = view.rollout;
          if (!r || r.nodes.length === 0) {
            info(
              c.green(`Every selected worker already runs ${view.controller.id}.`, process.stderr),
            );
            return;
          }
          if (!opts.wait) {
            process.stdout.write(rolloutLines(r).join('\n') + '\n');
            info(
              c.dim(
                'Follow it with `gridstator agents update --wait` or `gridstator agents status`.',
                process.stderr,
              ),
            );
            return;
          }
          const final = await follow(client);
          if (final?.state === 'halted') exitState.code = 1;
        }),
    );

  agents
    .command('cancel')
    .description('Stop the running rollout; the worker being updated right now finishes (admin)')
    .action(async () =>
      withClient(async (client) => {
        const view = await client.cancelUpdate();
        if (view.rollout) process.stdout.write(rolloutLines(view.rollout).join('\n') + '\n');
      }),
    );

  agents
    .command('set')
    .description(
      'Set auto-update and toolchain options: for one worker, or the cluster default (no node)',
    )
    .argument('[node]', 'node name or id; omit to change the cluster default')
    .option(
      '--auto-update <mode>',
      'off | follow | window, or inherit (node only: use the cluster default)',
    )
    .option('--window <HH:MM-HH:MM>', 'daily update window in the controller’s local time')
    .option('--act <version>', 'act for self-updating workers: a version, "pinned" or "off"')
    .option('--node <version>', 'Node.js for self-updating workers: a version, "pinned" or "off"')
    .action(
      async (
        node: string | undefined,
        opts: { autoUpdate?: string; window?: string; act?: string; node?: string },
      ) =>
        withClient(async (client) => {
          if (node) {
            if (opts.window || opts.act || opts.node)
              throw new GridstatorError(
                'BAD_REQUEST',
                'The update window and the toolchain are set for the whole cluster',
                { hint: 'Run `gridstator agents set` without a node.' },
              );
            if (!opts.autoUpdate)
              throw new GridstatorError('BAD_REQUEST', 'Nothing to change', {
                hint: 'Pass --auto-update off|follow|window|inherit.',
              });
            const mode = NodeAutoUpdateSchema.safeParse(opts.autoUpdate);
            if (!mode.success)
              throw new GridstatorError('BAD_REQUEST', `Invalid mode "${opts.autoUpdate}"`, {
                hint: 'Use off, follow, window or inherit.',
              });
            const n = await client.updateNode(node, { autoUpdate: mode.data });
            info(
              `${n.name}: auto-update ${n.settings.autoUpdate}${n.update ? ` (effective: ${n.update.autoUpdate})` : ''}.`,
            );
            return;
          }
          const patch: Record<string, unknown> = {};
          if (opts.autoUpdate) {
            const mode = AutoUpdateModeSchema.safeParse(opts.autoUpdate);
            if (!mode.success)
              throw new GridstatorError('BAD_REQUEST', `Invalid mode "${opts.autoUpdate}"`, {
                hint: 'Use off, follow or window (inherit only applies to a node).',
              });
            patch['autoUpdate'] = mode.data;
          }
          if (opts.window) patch['updateWindow'] = parseWindow(opts.window);
          if (opts.act || opts.node) {
            const current = await client.request<ClusterSettings>('GET', '/v1/settings');
            const tool = (v: string | undefined, cur: ToolOverride): ToolOverride =>
              v === undefined ? cur : v === 'pinned' ? null : v;
            patch['toolchain'] = {
              act: tool(opts.act, current.toolchain?.act ?? null),
              node: tool(opts.node, current.toolchain?.node ?? null),
            };
          }
          if (Object.keys(patch).length === 0)
            throw new GridstatorError('BAD_REQUEST', 'Nothing to change', {
              hint: 'See `gridstator agents set --help`.',
            });
          const settings = await client.request<ClusterSettings>('PATCH', '/v1/settings', {
            json: patch,
          });
          const tc = settings.toolchain;
          info(
            `Cluster auto-update: ${settings.autoUpdate}${settings.updateWindow ? ` (window ${settings.updateWindow.start}-${settings.updateWindow.end})` : ''}; act ${tc?.act ?? 'pinned'}, Node.js ${tc?.node ?? 'pinned'}.`,
          );
        }),
    );
}

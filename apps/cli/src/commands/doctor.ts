import { execFile } from 'node:child_process';
import { stat } from 'node:fs/promises';
import { promisify } from 'node:util';
import type { Command } from 'commander';
import {
  defaultAgentDataDir,
  defaultClientConfigPath,
  defaultControllerDataDir,
  errorMessage,
} from '@gridstator/shared';
import { GridstatorClient, readClientConfig, resolveConnection } from '@gridstator/client';
import { readAgentState } from '@gridstator/agent';
import { DockerCli } from '@gridstator/executors';
import { findProject, loadProjectConfig } from '@gridstator/snapshot';
import { c, printJson } from '../output.js';
import { exitState } from './client.js';

const execFileAsync = promisify(execFile);

type Status = 'ok' | 'warn' | 'fail' | 'info';
interface Check {
  name: string;
  status: Status;
  detail: string;
  hint?: string;
}

async function mode(path: string): Promise<number | undefined> {
  try {
    return (await stat(path)).mode & 0o777;
  } catch {
    return undefined;
  }
}

export async function runDoctor(): Promise<Check[]> {
  const checks: Check[] = [];
  const add = (name: string, status: Status, detail: string, hint?: string) =>
    checks.push({ name, status, detail, ...(hint ? { hint } : {}) });

  const [major = 0, minor = 0] = process.versions.node.split('.').map(Number);
  add(
    'Node.js',
    major > 22 || (major === 22 && minor >= 19) ? 'ok' : 'fail',
    `v${process.versions.node}`,
    'Gridstator needs Node.js 22.19 or newer.',
  );

  try {
    const { stdout } = await execFileAsync('git', ['--version']);
    add('git', 'ok', stdout.trim());
  } catch {
    add(
      'git',
      'warn',
      'not found',
      'Without git, snapshots fall back to a .gitignore-aware directory walk.',
    );
  }

  const configPath = defaultClientConfigPath();
  const saved = await readClientConfig(configPath).catch((err: unknown) => {
    add('client config', 'fail', errorMessage(err));
    return undefined;
  });
  if (saved) {
    const m = await mode(configPath);
    add(
      'client config',
      m !== undefined && (m & 0o077) !== 0 ? 'warn' : 'ok',
      configPath,
      m !== undefined && (m & 0o077) !== 0
        ? `Permissions are ${m.toString(8)}; run chmod 600 ${configPath}`
        : undefined,
    );
  } else if (!checks.some((x) => x.name === 'client config')) {
    add(
      'client config',
      process.env['GRIDSTATOR_URL'] ? 'info' : 'warn',
      'not logged in',
      'Run `gridstator login <url> --fingerprint ... --token ...`.',
    );
  }

  let client: GridstatorClient | undefined;
  try {
    const conn = await resolveConnection(configPath);
    client = new GridstatorClient(conn);
    const health = await client.health();
    add('controller', 'ok', `${conn.url} (v${health.version}, TLS verified against pinned CA)`);
    const cluster = await client.cluster();
    add('authentication', 'ok', `signed in as "${cluster.whoami.name}" (${cluster.whoami.role})`);
    const nodes = await client.listNodes();
    const online = nodes.filter((n) => n.status === 'online');
    add(
      'nodes',
      online.length > 0 ? 'ok' : 'warn',
      `${online.length}/${nodes.length} online`,
      online.length === 0
        ? 'Start an agent (`gridstator agent start`) or `controller start --with-agent`.'
        : undefined,
    );
    if (cluster.build) {
      const behind = nodes.filter(
        (n) => n.update && n.update.state !== 'current' && n.update.state !== 'unknown',
      );
      const known = nodes.filter((n) => n.update && n.update.state !== 'unknown');
      // A behind worker is a warning, not a failure: `agents status --check` is the hard gate.
      add(
        'worker versions',
        behind.length === 0 ? 'ok' : 'warn',
        behind.length === 0
          ? `${known.length} reachable worker(s) run controller build ${cluster.build.id}`
          : `${behind.length} of ${known.length} reachable worker(s) not on ${cluster.build.id}: ${behind
              .map((n) => `${n.name} (${n.update?.state})`)
              .join(', ')}`,
        behind.length > 0 ? 'See `gridstator agents status`.' : undefined,
      );
    }
    for (const n of nodes) {
      if (n.status !== 'online') continue;
      if (n.health === 'degraded') add(`node ${n.name}`, 'warn', n.healthIssues.join('; '));
      else if (n.settings.schedulingMode !== 'enabled')
        add(`node ${n.name}`, 'info', `scheduling ${n.settings.schedulingMode}`);
      else
        add(
          `node ${n.name}`,
          'ok',
          `${n.info?.cpus ?? '?'} CPU, docker ${n.info?.docker.version ?? '?'}, ${n.activeJobs.length}/${n.settings.maxConcurrency} jobs`,
        );
    }
  } catch (err) {
    const e = err as { message: string; hint?: string };
    add('controller', 'fail', e.message, e.hint);
  } finally {
    client?.close();
  }

  const docker = await new DockerCli().probe();
  add(
    'local docker',
    docker.available ? 'ok' : 'info',
    docker.available ? `v${docker.version}` : (docker.error ?? 'unavailable'),
    docker.available ? undefined : 'Only needed if this machine runs an agent.',
  );

  const agentState = await readAgentState(defaultAgentDataDir()).catch(() => undefined);
  if (agentState)
    add('local agent', 'info', `joined as "${agentState.name}" → ${agentState.controllerUrl}`);
  const ctrlMode = await mode(defaultControllerDataDir());
  if (ctrlMode !== undefined) {
    add(
      'local controller data',
      (ctrlMode & 0o077) !== 0 ? 'warn' : 'ok',
      defaultControllerDataDir(),
      (ctrlMode & 0o077) !== 0
        ? `Permissions are ${ctrlMode.toString(8)}; run chmod 700 on it.`
        : undefined,
    );
  }

  try {
    const loc = await findProject(process.cwd());
    if (loc.configPath) {
      await loadProjectConfig(loc.configPath);
      add('project config', 'ok', loc.configPath);
    } else {
      add('project', 'info', `${loc.root} (no gridstator.yml; defaults apply)`);
    }
  } catch (err) {
    add('project config', 'fail', errorMessage(err));
  }
  return checks;
}

export function registerDoctorCommand(program: Command): void {
  program
    .command('doctor')
    .description("Check this machine's setup and the connection to the controller")
    .option('--json', 'machine-readable output')
    .action(async (opts: { json?: boolean }) => {
      const checks = await runDoctor();
      if (opts.json) printJson(checks);
      else {
        const icon: Record<Status, string> = {
          ok: c.green('✓'),
          warn: c.yellow('!'),
          fail: c.red('✗'),
          info: c.blue('i'),
        };
        for (const ch of checks) {
          process.stdout.write(`${icon[ch.status]} ${ch.name.padEnd(22)} ${ch.detail}\n`);
          if (ch.hint && ch.status !== 'ok')
            process.stdout.write(`  ${''.padEnd(22)} ${c.dim(ch.hint)}\n`);
        }
      }
      if (checks.some((ch) => ch.status === 'fail')) exitState.code = 1;
    });
}

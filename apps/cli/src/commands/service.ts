import { execFile } from 'node:child_process';
import { existsSync, realpathSync } from 'node:fs';
import { chown, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { promisify } from 'node:util';
import type { Command } from 'commander';
import {
  activateVersion,
  bundledPackageDir,
  defaultAgentDataDir,
  defaultControllerDataDir,
  GridstatorError,
  installFromPackageDir,
  installRoot,
  launcherPath,
  writeLauncher,
} from '@gridstator/shared';
import { c, info } from '../output.js';

const execFileAsync = promisify(execFile);

export type ServiceRole = 'controller' | 'agent';

export interface UnitOptions {
  role: ServiceRole;
  /** Absolute path of the Node.js binary. */
  node: string;
  /** Absolute path of the gridstator entry script. */
  script: string;
  /** Extra arguments after `controller start` / `agent start`. */
  args: string[];
  /** System units run as this user; user units omit it. */
  user?: { name: string; home: string };
  pathDirs: string[];
}

export function unitName(role: ServiceRole): string {
  return `gridstator-${role}.service`;
}

/** Renders a systemd unit for the controller or an agent. */
export function renderUnit(o: UnitOptions): string {
  const quote = (a: string) =>
    /^[A-Za-z0-9_./:=@%+-]+$/.test(a) ? a : `"${a.replace(/(["\\$`])/g, '\\$1')}"`;
  const exec = [o.node, o.script, o.role, 'start', ...o.args].map(quote).join(' ');
  const lines = [
    '[Unit]',
    `Description=Gridstator ${o.role === 'controller' ? 'controller' : 'worker agent'}`,
    'Documentation=https://github.com/jepsenphil/gridstator',
    'After=network-online.target docker.service',
    'Wants=network-online.target',
    '',
    '[Service]',
    'Type=simple',
    ...(o.user
      ? [
          `User=${o.user.name}`,
          `Group=${o.user.name}`,
          'SupplementaryGroups=docker',
          `WorkingDirectory=${o.user.home}`,
        ]
      : []),
    `ExecStart=${exec}`,
    `Environment=PATH=${[...new Set(o.pathDirs)].join(':')}`,
    'Environment=GRIDSTATOR_LOG_FORMAT=json',
    'Restart=on-failure',
    'RestartSec=5',
    // Agents stop jobs cleanly on SIGTERM; give them time.
    'TimeoutStopSec=90',
    '',
    '[Install]',
    `WantedBy=${o.user ? 'multi-user.target' : 'default.target'}`,
    '',
  ];
  return lines.join('\n');
}

interface SystemUser {
  name: string;
  home: string;
  uid: number;
  gid: number;
}

async function lookupUser(name: string): Promise<SystemUser> {
  const passwd = await readFile('/etc/passwd', 'utf8');
  for (const line of passwd.split('\n')) {
    const f = line.split(':');
    if (f[0] === name && f[5]) return { name, home: f[5], uid: Number(f[2]), gid: Number(f[3]) };
  }
  throw new GridstatorError('BAD_REQUEST', `User ${name} does not exist`);
}

/** Hands a tree created by root (sudo install-service --system) to the service user. */
async function chownTree(path: string, uid: number, gid: number): Promise<void> {
  await chown(path, uid, gid);
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const p = join(path, entry.name);
    if (entry.isDirectory()) await chownTree(p, uid, gid);
    else await chown(p, uid, gid);
  }
}

async function systemctl(args: string[]): Promise<void> {
  try {
    await execFileAsync('systemctl', args);
  } catch (err) {
    const e = err as { stderr?: string; message: string };
    throw new GridstatorError(
      'INTERNAL',
      `systemctl ${args.join(' ')} failed: ${(e.stderr ?? e.message).trim()}`,
    );
  }
}

interface InstallFlags {
  system?: boolean;
  user?: string;
  print?: boolean;
  force?: boolean;
  dataDir?: string;
  withAgent?: boolean;
  agentName?: string;
}

function ensureSystemd(): void {
  if (process.platform !== 'linux') {
    throw new GridstatorError(
      'BAD_REQUEST',
      'install-service manages systemd services on Linux (including WSL2 with systemd enabled)',
      {
        hint: 'On other systems run `gridstator controller start` / `gridstator agent start` with your service manager of choice.',
      },
    );
  }
}

async function install(role: ServiceRole, flags: InstallFlags): Promise<void> {
  ensureSystemd();
  const isRoot = typeof process.getuid === 'function' && process.getuid() === 0;
  if (flags.system && !isRoot && !flags.print) {
    throw new GridstatorError(
      'FORBIDDEN',
      '--system installs into /etc/systemd/system and needs root',
      { hint: 'Run it with sudo, or omit --system for a user service.' },
    );
  }
  const targetUser = flags.system
    ? await lookupUser(flags.user ?? process.env['SUDO_USER'] ?? process.env['USER'] ?? 'root')
    : undefined;
  const home = targetUser?.home ?? homedir();
  const script = realpathSync(process.argv[1] ?? '');
  const args: string[] = [];
  // System services run as another user than (sudo) root: pass the data directory explicitly.
  const dataDir =
    flags.dataDir ?? (targetUser ? join(home, '.local', 'share', 'gridstator', role) : undefined);
  if (dataDir) args.push('--data-dir', dataDir);
  if (role === 'controller' && flags.withAgent)
    args.push('--with-agent', ...(flags.agentName ? ['--agent-name', flags.agentName] : []));
  // A bundled build is copied into the role's version store and started through the launcher, so
  // the service can update itself and roll back. The unbundled workspace build cannot be copied
  // (it needs node_modules) and is started in place.
  const pkgDir = bundledPackageDir(script);
  const root = installRoot(
    dataDir ?? (role === 'agent' ? defaultAgentDataDir() : defaultControllerDataDir()),
  );
  const unit = renderUnit({
    role,
    node: process.execPath,
    script: pkgDir ? launcherPath(root) : script,
    args,
    ...(targetUser ? { user: targetUser } : {}),
    pathDirs: [
      join(home, '.local', 'bin'),
      dirname(process.execPath),
      '/usr/local/bin',
      '/usr/bin',
      '/bin',
    ],
  });
  if (flags.print) {
    process.stdout.write(unit);
    return;
  }
  const dir = flags.system ? '/etc/systemd/system' : join(homedir(), '.config', 'systemd', 'user');
  const file = join(dir, unitName(role));
  if (existsSync(file) && !flags.force) {
    throw new GridstatorError('CONFLICT', `${file} already exists`, {
      hint: 'Pass --force to replace it (this also converts an existing service to a self-updating install).',
    });
  }
  if (pkgDir) {
    const created = await mkdir(root, { recursive: true });
    const id = await installFromPackageDir(root, pkgDir);
    await activateVersion(root, id);
    await writeLauncher(root);
    if (targetUser) await chownTree(created ?? root, targetUser.uid, targetUser.gid);
    info(`Installed version ${id} into ${root} (updates and rollbacks happen there).`);
  } else {
    info(
      c.yellow(
        `${script} is not a bundled build, so this service runs it in place and cannot update itself. Install the release tarball (or run \`pnpm bundle\` and use apps/cli/release) for self-updates.`,
        process.stderr,
      ),
    );
  }
  await mkdir(dir, { recursive: true });
  await writeFile(file, unit, { mode: 0o644 });
  const ctl = flags.system ? [] : ['--user'];
  await systemctl([...ctl, 'daemon-reload']);
  await systemctl([...ctl, 'enable', unitName(role)]);
  // restart (not start): with --force the old process must pick up the new unit and binary.
  await systemctl([...ctl, 'restart', unitName(role)]);
  info(c.green(`Installed and (re)started ${unitName(role)} (${file}).`, process.stderr));
  if (!flags.system) {
    // Without lingering, user services stop when the user logs out.
    try {
      await execFileAsync('loginctl', ['enable-linger']);
      info('Enabled lingering, so it keeps running when you are logged out and starts at boot.');
    } catch {
      info(
        c.yellow(
          `Run \`sudo loginctl enable-linger ${process.env['USER'] ?? '$USER'}\` so it keeps running when you are logged out.`,
          process.stderr,
        ),
      );
    }
  }
  const status = flags.system
    ? `sudo systemctl status ${unitName(role)}`
    : `systemctl --user status ${unitName(role)}`;
  const logs = flags.system
    ? `sudo journalctl -u ${unitName(role)} -f`
    : `journalctl --user -u ${unitName(role)} -f`;
  info(`Status: ${status}\nLogs:   ${logs}`);
}

async function uninstall(role: ServiceRole, flags: { system?: boolean }): Promise<void> {
  ensureSystemd();
  const dir = flags.system ? '/etc/systemd/system' : join(homedir(), '.config', 'systemd', 'user');
  const file = join(dir, unitName(role));
  if (!existsSync(file)) throw new GridstatorError('NOT_FOUND', `${file} does not exist`);
  const ctl = flags.system ? [] : ['--user'];
  await systemctl([...ctl, 'disable', '--now', unitName(role)]).catch(() => undefined);
  await rm(file);
  await systemctl([...ctl, 'daemon-reload']);
  info(`Stopped and removed ${unitName(role)}. Data and credentials were kept.`);
}

export function registerServiceCommands(program: Command): void {
  for (const role of ['controller', 'agent'] as const) {
    const parent = program.commands.find((cmd) => cmd.name() === role);
    if (!parent) continue;
    const cmd = parent
      .command('install-service')
      .description(`Run the ${role} as a systemd service that starts at boot`)
      .option('--system', 'install a system service (needs sudo) instead of a user service')
      .option(
        '--user <name>',
        'with --system: the user the service runs as (default: the sudo user)',
      )
      .option('--data-dir <dir>', `${role} data directory`)
      .option('--print', 'print the unit file instead of installing it')
      .option('--force', 'replace an existing unit');
    if (role === 'controller') {
      cmd
        .option('--with-agent', 'also run a worker agent in the controller process')
        .option('--agent-name <name>', 'node name for that agent');
    }
    cmd.action((flags: InstallFlags) => install(role, flags));
    parent
      .command('uninstall-service')
      .description(`Stop and remove the ${role} systemd service (keeps data)`)
      .option('--system', 'remove the system service instead of the user service')
      .action((flags: { system?: boolean }) => uninstall(role, flags));
  }
}

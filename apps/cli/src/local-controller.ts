import { execFile } from 'node:child_process';
import { constants } from 'node:fs';
import { access, mkdir, readFile, rm, stat } from 'node:fs/promises';
import { homedir, hostname, userInfo } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { promisify } from 'node:util';
import {
  GridstatorClient,
  normalizeControllerUrl,
  readClientConfig,
  writeClientConfig,
} from '@gridstator/client';
import {
  Store,
  controllerPaths,
  issueClientTokenOffline,
  type ControllerFileConfig,
} from '@gridstator/controller';
import {
  GridstatorError,
  certificateFingerprint,
  defaultClientConfigPath,
  hashToken,
  readJsonFile,
  writeJsonFile,
} from '@gridstator/shared';
import { lanAddress } from './output.js';

const execFileAsync = promisify(execFile);
const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const UNIT = 'gridstator-controller.service';

export interface LocalControllerStatus {
  available: boolean;
  dataDir: string;
  hostname: string;
  clusterId: string | null;
  caFingerprint: string | null;
  host: string;
  port: number;
  url: string;
  aliases: Array<{ host: string; port: number }>;
  service: {
    installed: boolean;
    state: 'active' | 'inactive' | 'failed' | 'unknown';
    scope: 'user';
    canManage: boolean;
    reason: string | null;
  };
}

export interface LocalControllerOptions {
  dataDir: string;
  configPath?: string;
  /** Injected only by tests; production always uses the local user service manager. */
  execute?: (file: string, args: string[]) => Promise<{ stdout: string; stderr: string }>;
  verificationTimeoutMs?: number;
}

export interface LocalLoginResult {
  status: 'connected' | 'revoked' | 'unavailable' | 'differentCluster';
  configPath: string;
  url?: string;
  role?: 'admin';
  message?: string;
}

async function execute(options: LocalControllerOptions, args: string[]) {
  return options.execute
    ? options.execute('systemctl', ['--user', ...args])
    : execFileAsync('systemctl', ['--user', ...args], { timeout: 20_000 });
}

/** Parse systemd's displayed argument/environment words; never execute or expand their text. */
function systemdWords(value: string): string[] | undefined {
  const words: string[] = [];
  let word = '';
  let quoted: '"' | "'" | undefined;
  for (let index = 0; index < value.length; index++) {
    const char = value[index]!;
    if (char === '\\') {
      const next = value[++index];
      if (next === undefined) return undefined;
      if (next === 'x' && /^[\da-f]{2}$/i.test(value.slice(index + 1, index + 3))) {
        word += String.fromCharCode(Number.parseInt(value.slice(index + 1, index + 3), 16));
        index += 2;
      } else word += next;
    } else if (quoted) {
      if (char === quoted) quoted = undefined;
      else word += char;
    } else if (char === '"' || char === "'") quoted = char;
    else if (/\s/.test(char)) {
      if (word) {
        words.push(word);
        word = '';
      }
    } else word += char;
  }
  if (quoted) return undefined;
  if (word) words.push(word);
  return words;
}

function unitControllerDirectory(props: Record<string, string>): string | undefined {
  const argv = /argv\[\]=([\s\S]*?) ; (?:ignore_errors|start_time)=/.exec(
    props['ExecStart'] ?? '',
  )?.[1];
  if (!argv) return undefined;
  const args = systemdWords(argv);
  if (
    !args ||
    !args.some((arg, index) => arg === 'controller' && args[index + 1] === 'start') ||
    args.includes('-c') ||
    args.includes('--eval')
  )
    return undefined;
  const flag = args.findLastIndex((arg) => arg === '--data-dir' || arg.startsWith('--data-dir='));
  if (flag !== -1) {
    const arg = args[flag]!;
    const path = arg === '--data-dir' ? args[flag + 1] : arg.slice('--data-dir='.length);
    return path && isAbsolute(path) ? resolve(path) : undefined;
  }
  // Environment files and wrappers can select an otherwise invisible directory. Refuse them
  // unless --data-dir made the target explicit rather than guessing which service to restart.
  if (props['EnvironmentFiles']?.trim()) return undefined;
  const environment = systemdWords(props['Environment'] ?? '');
  if (!environment) return undefined;
  const variables = new Map(
    environment.map((entry): [string, string] => {
      const separator = entry.indexOf('=');
      return separator < 0 ? [entry, ''] : [entry.slice(0, separator), entry.slice(separator + 1)];
    }),
  );
  const data = variables.get('GRIDSTATOR_CONTROLLER_DATA');
  if (data) return isAbsolute(data) ? resolve(data) : undefined;
  const xdg = variables.get('XDG_DATA_HOME');
  if (xdg) return isAbsolute(xdg) ? join(xdg, 'gridstator', 'controller') : undefined;
  return join(homedir(), '.local', 'share', 'gridstator', 'controller');
}

async function serviceStatus(
  options: LocalControllerOptions,
): Promise<LocalControllerStatus['service']> {
  if (process.platform !== 'linux')
    return {
      installed: false,
      state: 'unknown',
      scope: 'user',
      canManage: false,
      reason: 'User service controls require Linux and systemd.',
    };
  try {
    const { stdout } = await execute(options, [
      'show',
      UNIT,
      '--property=LoadState',
      '--property=ActiveState',
      '--property=ExecStart',
      '--property=Environment',
      '--property=EnvironmentFiles',
    ]);
    const props = Object.fromEntries<string>(
      stdout
        .trim()
        .split('\n')
        .map((line): [string, string] => {
          const separator = line.indexOf('=');
          return separator < 0 ? [line, ''] : [line.slice(0, separator), line.slice(separator + 1)];
        }),
    );
    const state = props['ActiveState'];
    const directory = unitControllerDirectory(props);
    const installed = props['LoadState'] === 'loaded';
    const canManage = installed && directory === resolve(options.dataDir);
    return {
      installed,
      state: state === 'active' || state === 'inactive' || state === 'failed' ? state : 'unknown',
      scope: 'user',
      canManage,
      reason: !installed
        ? 'No controller user service is installed.'
        : canManage
          ? null
          : directory
            ? `The installed controller service uses another data directory (${directory}). Host controls for ${resolve(options.dataDir)} are disabled.`
            : 'Cannot verify which controller data directory the installed service uses. Install it with an explicit --data-dir before managing it here.',
    };
  } catch {
    return {
      installed: false,
      state: 'unknown',
      scope: 'user',
      canManage: false,
      reason: 'The controller user service could not be inspected.',
    };
  }
}

function localUrl(host: string, port: number): string {
  const address = host === '0.0.0.0' ? '127.0.0.1' : host === '::' ? '::1' : host;
  return normalizeControllerUrl(
    `https://${address.includes(':') ? `[${address}]` : address}:${port}`,
  );
}

function nonLoopbackUrl(input: string): string | undefined {
  try {
    const url = normalizeControllerUrl(input);
    const address = new URL(url).hostname.toLowerCase();
    if (
      address === 'localhost' ||
      address.endsWith('.localhost') ||
      address === '[::1]' ||
      address === '[::]' ||
      address === '0.0.0.0' ||
      /^127\./.test(address)
    )
      return undefined;
    return url;
  } catch {
    return undefined;
  }
}

async function localFiles(options: LocalControllerOptions) {
  const paths = controllerPaths(options.dataDir);
  await access(paths.db, constants.R_OK | constants.W_OK);
  await access(paths.config, constants.R_OK | constants.W_OK);
  const owner = await stat(paths.db);
  if (process.getuid && owner.uid !== process.getuid()) {
    throw new GridstatorError('FORBIDDEN', 'This account does not own the local controller');
  }
  const [config, caPem] = await Promise.all([
    readJsonFile<ControllerFileConfig>(paths.config),
    readFile(paths.pki.caCert, 'utf8'),
  ]);
  if (!config) throw new GridstatorError('CONFIG', 'The local controller has no configuration');
  return { paths, config, caPem, fingerprint: certificateFingerprint(caPem) };
}

/** Local ownership is checked before inspecting the database or exposing host controls. */
export async function localControllerStatus(
  options: LocalControllerOptions,
): Promise<LocalControllerStatus> {
  const service = await serviceStatus(options);
  const unavailable: LocalControllerStatus = {
    available: false,
    dataDir: options.dataDir,
    hostname: hostname(),
    clusterId: null,
    caFingerprint: null,
    host: '0.0.0.0',
    port: 7300,
    url: 'https://127.0.0.1:7300',
    aliases: [],
    service,
  };
  try {
    const { paths, config, fingerprint } = await localFiles(options);
    const saved = await readClientConfig(options.configPath ?? defaultClientConfigPath()).catch(
      () => undefined,
    );
    const publicUrl =
      config.publicUrl ??
      (saved?.caFingerprint === fingerprint ? nonLoopbackUrl(saved.url) : undefined) ??
      localUrl(lanAddress() ?? config.host, config.port);
    const store = new Store(paths.db);
    try {
      return {
        ...unavailable,
        available: true,
        clusterId: store.getMeta('cluster_id') ?? null,
        caFingerprint: fingerprint,
        host: config.host,
        port: config.port,
        url: publicUrl,
        aliases: config.aliases ?? [],
      };
    } finally {
      store.close();
    }
  } catch {
    return unavailable;
  }
}

/** Atomic directory creation serializes bootstrap across independent extension hosts. */
async function withLocalLock<T>(path: string, fn: () => Promise<T>): Promise<T> {
  const lock = `${path}.lock`;
  await mkdir(dirname(lock), { recursive: true, mode: 0o700 });
  const deadline = Date.now() + 30_000;
  for (;;) {
    try {
      await mkdir(lock, { mode: 0o700 });
      await writeJsonFile(join(lock, 'owner.json'), { pid: process.pid, createdAt: Date.now() });
      break;
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== 'EEXIST') throw err;
      const owner = await readJsonFile<{ pid: number; createdAt: number }>(
        join(lock, 'owner.json'),
      ).catch(() => undefined);
      if (owner) {
        try {
          process.kill(owner.pid, 0);
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code === 'ESRCH') {
            await rm(lock, { recursive: true, force: true });
            continue;
          }
        }
      } else {
        const info = await stat(lock).catch(() => undefined);
        if (info && Date.now() - info.mtimeMs > 30_000) {
          await rm(lock, { recursive: true, force: true });
          continue;
        }
      }
      if (Date.now() > deadline)
        throw new GridstatorError(
          'CONFLICT',
          'Another window is setting up local controller access; try again',
        );
      await delay(40);
    }
  }
  try {
    return await fn();
  } finally {
    await rm(lock, { recursive: true, force: true });
  }
}

/** Never emits secrets, replaces a foreign cluster, or recreates a revoked token implicitly. */
export async function ensureLocalLogin(
  options: LocalControllerOptions,
  restore = false,
): Promise<LocalLoginResult> {
  const configPath = options.configPath ?? defaultClientConfigPath();
  return withLocalLock(`${configPath}.local-login`, async () => {
    let local: Awaited<ReturnType<typeof localFiles>>;
    try {
      local = await localFiles(options);
    } catch {
      return {
        status: 'unavailable',
        configPath,
        message: 'No controller owned by this account was found',
      };
    }
    const saved = await readClientConfig(configPath);
    if (saved && saved.caFingerprint !== local.fingerprint) {
      return {
        status: 'differentCluster',
        configPath,
        message: 'The saved connection selects another cluster',
      };
    }
    if (saved) {
      const store = new Store(local.paths.db);
      try {
        const row = store.tokenByHash(hashToken(saved.token));
        const valid =
          row &&
          row.kind === 'client' &&
          row.revoked_at === null &&
          (row.expires_at === null || row.expires_at >= Date.now());
        if (!valid && !restore) {
          return {
            status: 'revoked',
            configPath,
            url: saved.url,
            message: 'Local access was revoked or expired. Use Restore Local Access to reconnect.',
          };
        }
        if (valid && row.role === 'admin')
          return { status: 'connected', configPath, url: saved.url, role: 'admin' };
      } finally {
        store.close();
      }
    }
    const issued = await issueClientTokenOffline({
      dataDir: options.dataDir,
      name: `vscode:${userInfo().username}`,
      role: 'admin',
    });
    const url = saved?.url ?? localUrl(local.config.host, local.config.port);
    await writeClientConfig(
      {
        version: 1,
        url,
        caPem: issued.caPem,
        caFingerprint: issued.fingerprint,
        token: issued.token,
        tokenName: `vscode:${userInfo().username}`,
      },
      configPath,
    );
    return { status: 'connected', configPath, url, role: 'admin' };
  });
}

async function localAdminClient(
  options: LocalControllerOptions,
  url?: string,
): Promise<GridstatorClient> {
  const result = await ensureLocalLogin(options);
  if (result.status !== 'connected')
    throw new GridstatorError(
      'FORBIDDEN',
      result.message ?? 'Restore local controller access first',
    );
  const saved = await readClientConfig(options.configPath ?? defaultClientConfigPath());
  if (!saved) throw new GridstatorError('CONFIG', 'Local controller login was not saved');
  const { config } = await localFiles(options);
  return new GridstatorClient({
    url: url ?? localUrl(config.host, config.port),
    ca: saved.caPem,
    token: saved.token,
  });
}

/** Prevents a service restart from abandoning jobs belonging to an embedded worker. */
async function protectEmbeddedWorker(
  options: LocalControllerOptions,
): Promise<() => Promise<void>> {
  const embedded = await readJsonFile<{ nodeId: string }>(
    join(options.dataDir, 'agent', 'agent.json'),
  );
  if (!embedded) return async () => {};
  const client = await localAdminClient(options);
  try {
    const node = (await client.listNodes()).find((n) => n.id === embedded.nodeId);
    if (!node) {
      client.close();
      return async () => {};
    }
    await client.updateNode(node.id, { schedulingMode: 'draining' });
    const drained = (await client.listNodes()).find((n) => n.id === node.id);
    if (drained?.activeJobs.length) {
      await client.updateNode(node.id, { schedulingMode: node.settings.schedulingMode });
      throw new GridstatorError(
        'CONFLICT',
        `Finish the ${drained.activeJobs.length} running job(s) on ${node.name} before restarting the controller`,
        { hint: 'The controller also runs this worker. No jobs were cancelled.' },
      );
    }
    return async () => {
      client.close();
      let restored: GridstatorClient | undefined;
      try {
        restored = await localAdminClient(options);
        await restored.updateNode(node.id, { schedulingMode: node.settings.schedulingMode });
      } catch {
        // A deliberate Stop leaves the API unavailable. Restore the persisted preference so a
        // later Start resumes the previous scheduling mode rather than leaving a worker drained.
        const store = new Store(controllerPaths(options.dataDir).db);
        try {
          const current = store.nodeById(node.id);
          if (current)
            store.updateNode(node.id, {
              settings: { ...current.settings, schedulingMode: node.settings.schedulingMode },
            });
        } finally {
          store.close();
        }
      } finally {
        restored?.close();
      }
    };
  } catch (err) {
    client.close();
    throw err;
  }
}

export async function localControllerService(
  options: LocalControllerOptions,
  action: 'start' | 'stop' | 'restart',
): Promise<LocalControllerStatus> {
  return withLocalLock(join(options.dataDir, 'host-operation'), async () => {
    const status = await localControllerStatus(options);
    if (!status.available)
      throw new GridstatorError('FORBIDDEN', 'This account does not own a local controller');
    if (!status.service.installed)
      throw new GridstatorError('CONFIG', 'No controller user service is installed', {
        hint: 'Run gridstator controller install-service on this host first.',
      });
    if (!status.service.canManage)
      throw new GridstatorError(
        'FORBIDDEN',
        status.service.reason ?? 'The installed service does not run this controller',
      );
    const restore =
      action !== 'start' && status.service.state === 'active'
        ? await protectEmbeddedWorker(options)
        : undefined;
    try {
      await execute(options, [action, UNIT]);
    } finally {
      if (restore) await restore();
    }
    return localControllerStatus(options);
  });
}

function aliasesCanCoexist(
  a: { host: string; port: number },
  b: { host: string; port: number },
): boolean {
  return (
    a.port !== b.port ||
    (a.host !== b.host &&
      !['0.0.0.0', '::'].includes(a.host) &&
      !['0.0.0.0', '::'].includes(b.host))
  );
}

export interface ConfigureLocalController {
  host: string;
  port: number;
  url: string;
}

/** Keep old listeners for offline/legacy workers; roll back service and config on any failed verification. */
export async function configureLocalController(
  options: LocalControllerOptions,
  input: ConfigureLocalController,
): Promise<LocalControllerStatus> {
  return withLocalLock(join(options.dataDir, 'host-operation'), async () => {
    if (
      !input.host ||
      /[\s/\\]/.test(input.host) ||
      !Number.isInteger(input.port) ||
      input.port < 1 ||
      input.port > 65535
    ) {
      throw new GridstatorError(
        'BAD_REQUEST',
        'Enter a listening address and a port from 1 to 65535',
      );
    }
    const url = normalizeControllerUrl(input.url);
    const parsedUrl = new URL(url);
    if (Number(parsedUrl.port || 443) !== input.port)
      throw new GridstatorError(
        'BAD_REQUEST',
        'The public URL must use the configured listening port',
      );
    if (['0.0.0.0', '[::]'].includes(parsedUrl.hostname))
      throw new GridstatorError(
        'BAD_REQUEST',
        'Workers need a reachable controller address, not a wildcard address',
      );
    const before = await localFiles(options);
    const status = await localControllerStatus(options);
    if (!status.service.installed)
      throw new GridstatorError(
        'CONFIG',
        'Install a controller user service before changing its address',
      );
    if (!status.service.canManage)
      throw new GridstatorError(
        'FORBIDDEN',
        status.service.reason ?? 'The installed service does not run this controller',
      );
    if (
      before.config.host !== input.host &&
      before.config.port === input.port &&
      !aliasesCanCoexist(before.config, input)
    ) {
      throw new GridstatorError(
        'CONFLICT',
        'This address change needs a different port so the old listener can keep offline workers connected',
        {
          hint: 'Choose a new port for this move. The previous address remains available for workers that have not reconnected yet.',
        },
      );
    }
    const savedPath = options.configPath ?? defaultClientConfigPath();
    let workers: Awaited<ReturnType<GridstatorClient['listNodes']>> = [];
    if (status.service.state === 'active') {
      const client = await localAdminClient(options);
      try {
        workers = await client.listNodes();
      } finally {
        client.close();
      }
    } else {
      // Apply & Restart is an explicit request to bring a stopped service up. Merely opening
      // a window or inspecting Settings still never starts it.
      const result = await ensureLocalLogin(options);
      if (result.status !== 'connected')
        throw new GridstatorError('FORBIDDEN', result.message ?? 'Restore local access first');
    }
    const restore =
      status.service.state === 'active' ? await protectEmbeddedWorker(options) : async () => {};
    const saved = await readClientConfig(savedPath);
    if (!saved) throw new GridstatorError('CONFIG', 'Local controller login is missing');
    const aliases = [...(before.config.aliases ?? [])];
    const old = { host: before.config.host, port: before.config.port };
    if (
      (old.host !== input.host || old.port !== input.port) &&
      !aliases.some((a) => a.host === old.host && a.port === old.port)
    )
      aliases.push(old);
    const next: ControllerFileConfig = {
      ...before.config,
      host: input.host,
      port: input.port,
      publicUrl: url,
      aliases: aliases.filter((a) => a.host !== input.host || a.port !== input.port),
    };
    if (next.aliases?.some((alias) => !aliasesCanCoexist(alias, input))) {
      await restore();
      throw new GridstatorError(
        'CONFLICT',
        'The new listener conflicts with a retained worker endpoint',
        { hint: 'Choose a different port so offline workers can reconnect safely.' },
      );
    }
    let applied = false;
    try {
      await writeJsonFile(before.paths.config, next);
      applied = true;
      await execute(options, ['restart', UNIT]);
      const verify = new GridstatorClient({ url, ca: before.caPem, token: saved.token });
      try {
        const deadline = Date.now() + (options.verificationTimeoutMs ?? 45_000);
        for (;;) {
          const view = await verify.cluster().catch(() => undefined);
          if (view?.clusterId === status.clusterId) {
            const current = await verify.listNodes();
            const outstanding = workers
              .filter((n) => n.status === 'online' && n.info?.controllerUrl !== undefined)
              .filter((n) => {
                const reconnected = current.find((candidate) => candidate.id === n.id);
                return reconnected?.status !== 'online' || reconnected.info?.controllerUrl !== url;
              });
            if (outstanding.length === 0) break;
          }
          if (Date.now() >= deadline)
            throw new GridstatorError(
              'UNAVAILABLE',
              'The controller or its workers did not reconnect at the new address',
            );
          await delay(200);
        }
      } finally {
        verify.close();
      }
      await writeClientConfig({ ...saved, url: localUrl(input.host, input.port) }, savedPath);
    } catch (err) {
      if (applied) {
        await writeJsonFile(before.paths.config, before.config);
        await execute(options, ['restart', UNIT]).catch(() => undefined);
      }
      throw new GridstatorError(
        'UNAVAILABLE',
        `The address change failed and the previous configuration was restored: ${(err as Error).message}`,
        { cause: err },
      );
    } finally {
      await restore().catch(() => undefined);
    }
    return localControllerStatus(options);
  });
}

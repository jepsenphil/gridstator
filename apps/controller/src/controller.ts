import { existsSync } from 'node:fs';
import { rename } from 'node:fs/promises';
import { join } from 'node:path';
import type { FastifyInstance } from 'fastify';
import {
  GRIDSTATOR_VERSION,
  GridstatorError,
  detectBuild,
  ensureDir,
  newId,
  readJsonFile,
  silentLogger,
  writeJsonFile,
  type BuildIdentity,
  type Logger,
} from '@gridstator/shared';
import type { NodeInfo, NodeSettingsPatch } from '@gridstator/protocol';
import { BlobStore } from '@gridstator/snapshot';
import { Auth } from './auth.js';
import { BuildSource } from './build.js';
import { Cluster, type ClusterTiming } from './cluster.js';
import { Store } from './db.js';
import { EventHub } from './hub.js';
import { LogStore } from './logs.js';
import { generatePki, loadPki, pkiPaths } from './pki.js';
import { runRetention } from './retention.js';
import { buildServer } from './server.js';
import { SelfUpdater, recoverAfterRollback } from './self-update.js';
import { SettingsService } from './settings.js';
import { ToolchainMirror, type Fetcher } from './toolchain.js';
import { UpdateManager, type UpdateTiming } from './updates.js';

export const DEFAULT_CONTROLLER_PORT = 7300;

export interface ControllerFileConfig {
  version: 1;
  host: string;
  port: number;
}

export function controllerPaths(dataDir: string) {
  return {
    config: join(dataDir, 'controller.json'),
    db: join(dataDir, 'controller.db'),
    cas: join(dataDir, 'cas'),
    logs: join(dataDir, 'logs'),
    pki: pkiPaths(dataDir),
  };
}

export interface InitResult {
  clusterId: string;
  fingerprint: string;
  adminToken: string;
  caPem: string;
  host: string;
  port: number;
  backupDir: string | undefined;
}

/**
 * Creates a new cluster in `dataDir`: CA + server certificate, database, and an admin token.
 * With `force`, an existing cluster is moved aside to `<dataDir>.bak-<timestamp>` (never deleted).
 */
export async function initController(options: {
  dataDir: string;
  host?: string;
  port?: number;
  force?: boolean;
  extraNames?: string[];
}): Promise<InitResult> {
  const paths = controllerPaths(options.dataDir);
  let backupDir: string | undefined;
  if (existsSync(paths.db) || existsSync(paths.pki.caCert)) {
    if (!options.force) {
      throw new GridstatorError(
        'CONFLICT',
        `A controller is already initialized in ${options.dataDir}`,
        {
          hint: 'Start it with `gridstator controller start`, or pass --force to create a new cluster (the old one is backed up).',
        },
      );
    }
    backupDir = `${options.dataDir}.bak-${new Date().toISOString().replace(/[:.]/g, '-')}`;
    await rename(options.dataDir, backupDir);
  }
  await ensureDir(options.dataDir);
  const clusterId = newId();
  const pki = await generatePki(options.dataDir, clusterId, options.extraNames ?? []);
  const store = new Store(paths.db);
  try {
    store.setMeta('cluster_id', clusterId);
    store.setMeta('created_at', String(Date.now()));
    const auth = new Auth(store);
    const { token } = auth.issue({ kind: 'client', name: 'admin', role: 'admin' });
    const host = options.host ?? '0.0.0.0';
    const port = options.port ?? DEFAULT_CONTROLLER_PORT;
    await writeJsonFile(
      paths.config,
      { version: 1, host, port } satisfies ControllerFileConfig,
      0o600,
    );
    return {
      clusterId,
      fingerprint: pki.fingerprint,
      adminToken: token,
      caPem: pki.caPem,
      host,
      port,
      backupDir,
    };
  } finally {
    store.close();
  }
}

/**
 * Issues a client token by writing to the controller's database directly (works while the
 * controller runs). For recovery when no admin token is at hand: access to the data directory is
 * already full control of the cluster.
 */
export async function issueClientTokenOffline(options: {
  dataDir: string;
  name: string;
  role: 'admin' | 'operator';
}): Promise<{ token: string; url: string; caPem: string; fingerprint: string }> {
  const paths = controllerPaths(options.dataDir);
  if (!existsSync(paths.db) || !existsSync(paths.pki.caCert)) {
    throw new GridstatorError('CONFIG', `No controller is initialized in ${options.dataDir}`);
  }
  const fileConfig = await readJsonFile<ControllerFileConfig>(paths.config);
  const pki = await loadPki(options.dataDir);
  const store = new Store(paths.db);
  try {
    const { token } = new Auth(store).issue({
      kind: 'client',
      name: options.name,
      role: options.role,
    });
    return {
      token,
      url: `https://127.0.0.1:${fileConfig?.port ?? DEFAULT_CONTROLLER_PORT}`,
      caPem: pki.caPem,
      fingerprint: pki.fingerprint,
    };
  } finally {
    store.close();
  }
}

export interface ControllerOptions {
  dataDir: string;
  /** Overrides controller.json. */
  host?: string;
  port?: number;
  logger?: Logger;
  timing?: Partial<ClusterTiming>;
  maxLogBytesPerJob?: number;
  retentionIntervalMs?: number;
  /** The build workers update to; default: the running process's build. */
  build?: BuildIdentity;
  updateTiming?: Partial<UpdateTiming>;
  /** Asked to stop and exit for a restart after switching to a published build. */
  onRestart?: () => void;
  /** Downloads toolchain archives (default: HTTPS from nodejs.org / GitHub). */
  toolchainFetcher?: Fetcher;
  toolchainPins?: ConstructorParameters<typeof ToolchainMirror>[0]['pins'];
}

export interface RunningController {
  url: string;
  host: string;
  port: number;
  clusterId: string;
  fingerprint: string;
  caPem: string;
  cluster: Cluster;
  store: Store;
  updates: UpdateManager;
  build: BuildSource;
  app: FastifyInstance;
  /** Registers (or re-uses) a node for an agent running in this process (embedded mode). */
  registerLocalAgent(
    name: string,
    info: NodeInfo,
    settings?: NodeSettingsPatch,
  ): { nodeId: string; credential: string };
  runRetention(): ReturnType<typeof runRetention>;
  close(): Promise<void>;
}

export async function startController(options: ControllerOptions): Promise<RunningController> {
  const logger = options.logger ?? silentLogger();
  const paths = controllerPaths(options.dataDir);
  if (!existsSync(paths.db) || !existsSync(paths.pki.caCert)) {
    throw new GridstatorError('CONFIG', `No controller is initialized in ${options.dataDir}`, {
      hint: 'Run `gridstator controller init` first.',
    });
  }
  const fileConfig = await readJsonFile<ControllerFileConfig>(paths.config);
  const host = options.host ?? fileConfig?.host ?? '0.0.0.0';
  const port = options.port ?? fileConfig?.port ?? DEFAULT_CONTROLLER_PORT;
  const pki = await loadPki(options.dataDir);
  const identity = options.build ?? (await detectBuild(GRIDSTATOR_VERSION));
  await recoverAfterRollback(options.dataDir, paths.db, identity, logger);
  const store = new Store(paths.db);
  const clusterId = store.getMeta('cluster_id') ?? 'unknown';
  const blobs = new BlobStore(paths.cas);
  await blobs.init();
  const logs = new LogStore(paths.logs, options.maxLogBytesPerJob ?? 64 * 1024 * 1024);
  const hub = new EventHub();
  const auth = new Auth(store);
  const settings = new SettingsService(store);
  const build = new BuildSource(identity, options.dataDir, logger.child({ component: 'updates' }));
  const toolchain = new ToolchainMirror({
    dir: join(options.dataDir, 'toolchain'),
    settings: () => settings.get(),
    logger: logger.child({ component: 'toolchain' }),
    ...(options.toolchainFetcher ? { fetcher: options.toolchainFetcher } : {}),
    ...(options.toolchainPins ? { pins: options.toolchainPins } : {}),
    onReady: () => updates.tick(),
  });
  const updates = new UpdateManager({
    store,
    build,
    settings: () => settings.get(),
    logger: logger.child({ component: 'updates' }),
    ...(options.updateTiming ? { timing: options.updateTiming } : {}),
    toolchain,
  });
  const selfUpdater = new SelfUpdater({
    dataDir: options.dataDir,
    build: identity,
    store,
    logger: logger.child({ component: 'self-update' }),
    ...(options.onRestart ? { onRestart: options.onRestart } : {}),
  });
  const cluster = new Cluster({
    store,
    logs,
    hub,
    auth,
    logger: logger.child({ component: 'cluster' }),
    ...(options.timing ? { timing: options.timing } : {}),
    updates,
  });
  const app = await buildServer({
    store,
    cluster,
    auth,
    hub,
    logs,
    blobs,
    settings,
    updates,
    build,
    toolchain,
    selfUpdater,
    logger,
    clusterId,
    caPem: pki.caPem,
    fingerprint: pki.fingerprint,
    tls: { key: pki.serverKeyPem, cert: pki.serverChainPem },
  });

  const retention = () =>
    runRetention({ store, logs, logsDir: paths.logs, blobs, settings: settings.get(), logger });
  try {
    await app.listen({ host, port });
  } catch (err) {
    store.close();
    const code = (err as NodeJS.ErrnoException).code;
    if (code === 'EADDRINUSE') {
      throw new GridstatorError('CONFLICT', `Port ${port} is already in use`, {
        hint: 'Is another controller running? Choose a different port with --port.',
      });
    }
    throw err;
  }
  const address = app.server.address();
  const boundPort = typeof address === 'object' && address ? address.port : port;
  cluster.start();
  void build.prepare().then(() => updates.tick());
  await selfUpdater
    .confirm()
    .catch((err: unknown) => logger.error({ err }, 'could not confirm the controller update'));
  const retentionTimer = setInterval(() => {
    retention()
      .then((r) => logger.info(r, 'retention sweep finished'))
      .catch((err: unknown) => logger.error({ err }, 'retention sweep failed'));
  }, options.retentionIntervalMs ?? 3600_000);
  retentionTimer.unref();
  logger.info(
    { host, port: boundPort, clusterId, fingerprint: pki.fingerprint },
    'controller listening',
  );

  let closed = false;
  return {
    url: `https://${host === '0.0.0.0' || host === '::' ? '127.0.0.1' : host}:${boundPort}`,
    host,
    port: boundPort,
    clusterId,
    fingerprint: pki.fingerprint,
    caPem: pki.caPem,
    cluster,
    store,
    updates,
    build,
    app,
    registerLocalAgent(name, info, nodeSettings) {
      const existing = store.nodeByName(name);
      if (existing) {
        const issued = auth.issue({ kind: 'agent', name: `agent:${name}`, nodeId: existing.id });
        return { nodeId: existing.id, credential: issued.token };
      }
      const reg = cluster.registerNode(name, info, nodeSettings, null);
      return { nodeId: reg.nodeId, credential: reg.credential };
    },
    runRetention: retention,
    async close() {
      if (closed) return;
      closed = true;
      clearInterval(retentionTimer);
      cluster.stop();
      // Do not leave a half-written update package behind.
      await build.prepare().catch(() => undefined);
      hub.closeAll();
      await app.close();
      store.close();
      logger.info('controller stopped');
    },
  };
}

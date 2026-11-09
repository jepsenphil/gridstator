import websocket from '@fastify/websocket';
import Fastify, { LogController, type FastifyInstance, type FastifyRequest } from 'fastify';
import type { WebSocket } from 'ws';
import type { z } from 'zod';
import { GridstatorError, GRIDSTATOR_VERSION, decodeWsData, type Logger } from '@gridstator/shared';
import {
  AgentMessageSchema,
  BlobDigestsRequestSchema,
  ClientEventMessageSchema,
  ClusterSettingsPatchSchema,
  CreateSnapshotRequestSchema,
  CreateTokenRequestSchema,
  ListJobsQuerySchema,
  LogsQuerySchema,
  PatchNodeRequestSchema,
  RegisterAgentRequestSchema,
  SubmitGroupRequestSchema,
  SubmitJobRequestSchema,
  isTerminal,
  parseManifest,
  type ClusterView,
  type HealthResponse,
  type JobEvent,
  type SnapshotView,
} from '@gridstator/protocol';
import { packBlobs, unpackBlobsInto, type BlobStore } from '@gridstator/snapshot';
import { tokenView, type Auth, type AuthRequirement, type Principal } from './auth.js';
import type { AgentConnection, Cluster } from './cluster.js';
import type { SnapshotRow, Store } from './db.js';
import type { EventHub } from './hub.js';
import type { LogStore } from './logs.js';
import type { SettingsService } from './settings.js';

declare module 'fastify' {
  interface FastifyContextConfig {
    auth?: AuthRequirement;
  }
  interface FastifyRequest {
    principal?: Principal;
  }
}

export interface ServerDeps {
  store: Store;
  cluster: Cluster;
  auth: Auth;
  hub: EventHub;
  logs: LogStore;
  blobs: BlobStore;
  settings: SettingsService;
  logger: Logger;
  clusterId: string;
  caPem: string;
  fingerprint: string;
  tls: { key: string; cert: string };
}

const MAX_UPLOAD_BYTES = 8 * 1024 * 1024 * 1024;

function parse<T>(schema: z.ZodType<T>, data: unknown, what: string): T {
  const r = schema.safeParse(data);
  if (!r.success) {
    const issues = r.error.issues
      .map((i) => `${i.path.join('.') || '(root)'}: ${i.message}`)
      .join('; ');
    throw new GridstatorError('BAD_REQUEST', `Invalid ${what}: ${issues}`);
  }
  return r.data;
}

function principal(req: FastifyRequest): Principal {
  if (!req.principal) throw new GridstatorError('UNAUTHORIZED', 'Not authenticated');
  return req.principal;
}

function snapshotView(s: SnapshotRow): SnapshotView {
  return {
    id: s.id,
    fileCount: s.file_count,
    totalBytes: s.total_bytes,
    createdAt: s.created_at,
    lastUsedAt: s.last_used_at,
    owner: s.owner,
  };
}

export async function buildServer(deps: ServerDeps): Promise<FastifyInstance> {
  const { store, cluster, auth, hub, logs, blobs, logger } = deps;
  const app = Fastify({
    https: { key: deps.tls.key, cert: deps.tls.cert, minVersion: 'TLSv1.2' },
    loggerInstance: logger.child({ component: 'http' }),
    logController: new LogController({ disableRequestLogging: true }),
    bodyLimit: 64 * 1024 * 1024,
    trustProxy: false,
  });

  await app.register(websocket, { options: { maxPayload: 64 * 1024 * 1024 } });

  app.addContentTypeParser('application/x-tar', (_req, payload, done) => done(null, payload));

  app.addHook('onRequest', async (req) => {
    const requirement = req.routeOptions.config.auth ?? 'client';
    const p = auth.authenticate(req.headers.authorization, requirement);
    if (p) req.principal = p;
  });

  app.addHook('onResponse', async (req, reply) => {
    req.log.debug(
      {
        method: req.method,
        url: req.url,
        status: reply.statusCode,
        ms: Math.round(reply.elapsedTime),
      },
      'request',
    );
  });

  app.setErrorHandler((err, req, reply) => {
    if (err instanceof GridstatorError) {
      if (err.status >= 500) req.log.error({ err }, 'request failed');
      return reply.status(err.status).send(err.toBody());
    }
    const status = (err as { statusCode?: number }).statusCode ?? 500;
    if (status >= 500) req.log.error({ err }, 'unhandled error');
    const code =
      status === 413
        ? 'PAYLOAD_TOO_LARGE'
        : status === 404
          ? 'NOT_FOUND'
          : status < 500
            ? 'BAD_REQUEST'
            : 'INTERNAL';
    const message =
      status < 500 ? (err as Error).message : 'Internal controller error (see controller logs)';
    return reply.status(status).send(new GridstatorError(code, message).toBody());
  });

  app.setNotFoundHandler((req, reply) => {
    reply
      .status(404)
      .send(new GridstatorError('NOT_FOUND', `No route for ${req.method} ${req.url}`).toBody());
  });

  // ---- public ------------------------------------------------------------------------------

  app.get('/v1/health', { config: { auth: 'none' } }, async (): Promise<HealthResponse> => ({
    ok: true,
    service: 'gridstator-controller',
    version: GRIDSTATOR_VERSION,
    apiVersion: 'v1',
  }));

  app.get('/v1/pki/ca.pem', { config: { auth: 'none' } }, async (_req, reply) => {
    reply.type('application/x-pem-file').send(deps.caPem);
  });

  // ---- cluster -----------------------------------------------------------------------------

  app.get('/v1/cluster', async (req): Promise<ClusterView> => {
    const p = principal(req);
    const nodes = cluster.listNodes();
    return {
      clusterId: deps.clusterId,
      version: GRIDSTATOR_VERSION,
      caFingerprint: deps.fingerprint,
      startedAt: cluster.startedAt,
      nodes: { total: nodes.length, online: nodes.filter((n) => n.status === 'online').length },
      jobs: store.countJobsByState(),
      whoami: { name: p.name, role: p.role ?? 'operator' },
    };
  });

  app.get('/v1/settings', async () => deps.settings.get());
  app.patch('/v1/settings', { config: { auth: 'admin' } }, async (req) =>
    deps.settings.update(parse(ClusterSettingsPatchSchema, req.body, 'settings')),
  );

  // ---- nodes -------------------------------------------------------------------------------

  app.get('/v1/nodes', async () => ({ nodes: cluster.listNodes() }));

  app.get<{ Params: { id: string } }>('/v1/nodes/:id', async (req) =>
    cluster.nodeView(cluster.findNode(req.params.id)),
  );

  app.patch<{ Params: { id: string } }>('/v1/nodes/:id', async (req) =>
    cluster.updateNodeSettings(
      req.params.id,
      parse(PatchNodeRequestSchema, req.body, 'node settings'),
    ),
  );

  app.delete<{ Params: { id: string }; Querystring: { force?: string } }>(
    '/v1/nodes/:id',
    { config: { auth: 'admin' } },
    async (req, reply) => {
      cluster.removeNode(req.params.id, req.query.force === 'true', principal(req).name);
      reply.status(204).send();
    },
  );

  app.post('/v1/agents/register', { config: { auth: 'join' } }, async (req) => {
    const body = parse(RegisterAgentRequestSchema, req.body, 'registration');
    return cluster.registerNode(body.name, body.info, body.settings, principal(req).tokenId);
  });

  app.get(
    '/v1/agent/ws',
    { websocket: true, config: { auth: 'agent' } },
    (socket: WebSocket, req) => {
      const nodeId = principal(req).nodeId;
      if (!nodeId) {
        socket.close(4003, 'agent credential is not bound to a node');
        return;
      }
      const conn: AgentConnection = {
        send: (msg) => {
          if (socket.readyState !== 1) return false;
          socket.send(JSON.stringify(msg));
          return true;
        },
        close: (code, reason) => socket.close(code, reason),
      };
      cluster.agentConnected(nodeId, conn);
      socket.on('message', (raw) => {
        let data: unknown;
        try {
          data = JSON.parse(decodeWsData(raw));
        } catch {
          logger.warn({ nodeId }, 'agent sent invalid JSON');
          return;
        }
        const msg = AgentMessageSchema.safeParse(data);
        if (!msg.success) {
          logger.warn(
            { nodeId, issues: msg.error.issues.slice(0, 3) },
            'agent sent an invalid message',
          );
          return;
        }
        try {
          cluster.agentMessage(nodeId, conn, msg.data);
        } catch (err) {
          logger.error({ err, nodeId, type: msg.data.type }, 'failed to handle agent message');
        }
      });
      socket.on('close', () => cluster.agentDisconnected(nodeId, conn));
      socket.on('error', (err) => logger.warn({ err, nodeId }, 'agent socket error'));
    },
  );

  // ---- jobs --------------------------------------------------------------------------------

  app.post('/v1/jobs', async (req, reply) => {
    const body = parse(SubmitJobRequestSchema, req.body, 'job');
    reply.status(201);
    return cluster.submit(body.spec, principal(req));
  });

  app.get('/v1/jobs', async (req) => {
    const q = parse(ListJobsQuerySchema, req.query, 'query');
    const nodeId = q.node ? cluster.findNode(q.node).id : undefined;
    const rows = store.listJobs({
      ...(q.state ? { states: q.state } : {}),
      ...(nodeId ? { nodeId } : {}),
      limit: q.limit,
      ...(q.before ? { before: q.before } : {}),
    });
    return { jobs: rows.map((r) => cluster.jobView(r)) };
  });

  app.get<{ Params: { id: string } }>('/v1/jobs/:id', async (req) =>
    cluster.jobView(cluster.getJob(req.params.id)),
  );

  app.get<{ Params: { id: string } }>('/v1/jobs/:id/events', async (req) => {
    cluster.getJob(req.params.id);
    const events: JobEvent[] = store.eventsForJob(req.params.id).map((e) => ({
      id: e.id,
      jobId: e.job_id,
      ts: e.ts,
      type: e.type,
      fromState: e.from_state,
      toState: e.to_state,
      actor: e.actor,
      message: e.message,
    }));
    return { events };
  });

  app.get<{ Params: { id: string } }>('/v1/jobs/:id/logs', async (req) => {
    const job = cluster.getJob(req.params.id);
    const q = parse(LogsQuerySchema, req.query, 'query');
    const entries = logs.read(job.id, q.after, q.limit);
    const last = entries.length > 0 ? entries[entries.length - 1]!.s : q.after;
    return { entries, complete: isTerminal(job.state) && last >= job.log_seq };
  });

  app.post<{ Params: { id: string } }>('/v1/jobs/:id/cancel', async (req) =>
    cluster.cancel(req.params.id, principal(req).name),
  );

  app.post<{ Params: { id: string } }>('/v1/jobs/:id/retry', async (req, reply) => {
    reply.status(201);
    return cluster.retry(req.params.id, principal(req));
  });

  // ---- job groups (sharding) ----------------------------------------------------------------

  app.post('/v1/groups', async (req, reply) => {
    const body = parse(SubmitGroupRequestSchema, req.body, 'group');
    reply.status(201);
    return cluster.submitGroup(body.spec, body.shards, body.name, principal(req));
  });

  app.get<{ Querystring: { limit?: string; before?: string } }>('/v1/groups', async (req) => {
    const limit = Math.min(200, Math.max(1, Number(req.query.limit ?? 30) || 30));
    return { groups: store.listGroups(limit, req.query.before).map((g) => cluster.groupView(g)) };
  });

  app.get<{ Params: { id: string } }>('/v1/groups/:id', async (req) =>
    cluster.getGroup(req.params.id),
  );

  app.post<{ Params: { id: string } }>('/v1/groups/:id/cancel', async (req) =>
    cluster.cancelGroup(req.params.id, principal(req).name),
  );

  app.post<{ Params: { id: string } }>('/v1/groups/:id/retry', async (req, reply) => {
    reply.status(201);
    return cluster.retryGroup(req.params.id, principal(req));
  });

  app.get<{ Params: { id: string } }>('/v1/jobs/:id/artifacts', async (req) => {
    cluster.getJob(req.params.id);
    return { artifacts: store.artifactsForJob(req.params.id) };
  });

  app.get<{ Params: { id: string; '*': string } }>(
    '/v1/jobs/:id/artifacts/*',
    async (req, reply) => {
      cluster.getJob(req.params.id);
      const name = req.params['*'];
      const artifact = store.artifactsForJob(req.params.id).find((a) => a.name === name);
      if (!artifact)
        throw new GridstatorError('NOT_FOUND', `Job ${req.params.id} has no artifact "${name}"`);
      if (!(await blobs.has(artifact.digest))) {
        throw new GridstatorError(
          'NOT_FOUND',
          `Artifact "${name}" is no longer stored on the controller`,
        );
      }
      const file = name.split('/').pop() ?? 'artifact';
      return reply
        .type('application/octet-stream')
        .header('content-length', artifact.size)
        .header('content-disposition', `attachment; filename="${file.replace(/["\\\r\n]/g, '_')}"`)
        .send(blobs.read(artifact.digest));
    },
  );

  // ---- blobs & snapshots -------------------------------------------------------------------

  app.post('/v1/blobs/missing', { config: { auth: 'client-or-agent' } }, async (req) => {
    const body = parse(BlobDigestsRequestSchema, req.body, 'digests');
    return { missing: await blobs.missing(body.digests) };
  });

  app.post(
    '/v1/blobs/upload',
    { bodyLimit: MAX_UPLOAD_BYTES, config: { auth: 'client-or-agent' } },
    async (req) => {
      if (!req.headers['content-type']?.startsWith('application/x-tar')) {
        throw new GridstatorError('BAD_REQUEST', 'Expected an application/x-tar body');
      }
      return unpackBlobsInto(req.body as AsyncIterable<Uint8Array>, blobs, {
        maxBytes: MAX_UPLOAD_BYTES,
        maxEntries: 1_000_000,
      });
    },
  );

  app.post('/v1/blobs/download', { config: { auth: 'client-or-agent' } }, async (req, reply) => {
    const body = parse(BlobDigestsRequestSchema, req.body, 'digests');
    const missing = await blobs.missing(body.digests);
    if (missing.length > 0)
      throw new GridstatorError('NOT_FOUND', `${missing.length} blob(s) not found`, {
        details: missing.slice(0, 10),
      });
    const sources = await Promise.all(
      [...new Set(body.digests)].map(async (digest) => ({
        digest,
        size: await blobs.size(digest),
        open: () => blobs.read(digest),
      })),
    );
    return reply.type('application/x-tar').send(packBlobs(sources));
  });

  app.get<{ Params: { digest: string } }>(
    '/v1/blobs/:digest',
    { config: { auth: 'client-or-agent' } },
    async (req, reply) => {
      const data = await blobs.readBuffer(req.params.digest);
      return reply.type('application/octet-stream').send(data);
    },
  );

  app.post('/v1/snapshots', async (req, reply) => {
    const { manifestDigest } = parse(CreateSnapshotRequestSchema, req.body, 'snapshot');
    let manifest;
    try {
      manifest = parseManifest(await blobs.readBuffer(manifestDigest));
    } catch (err) {
      if (err instanceof GridstatorError) throw err;
      throw new GridstatorError(
        'BAD_REQUEST',
        `Manifest ${manifestDigest} is invalid: ${(err as Error).message}`,
      );
    }
    const files = manifest.entries.flatMap((e) => (e.type === 'file' ? [e] : []));
    const missing = await blobs.missing(files.map((f) => f.digest));
    if (missing.length > 0) {
      throw new GridstatorError(
        'BAD_REQUEST',
        `Snapshot references ${missing.length} blob(s) that were not uploaded`,
        { details: missing.slice(0, 10) },
      );
    }
    const now = Date.now();
    const row = store.upsertSnapshot({
      id: manifestDigest,
      file_count: files.length,
      total_bytes: files.reduce((n, f) => n + f.size, 0),
      owner: principal(req).name,
      created_at: now,
      last_used_at: now,
    });
    reply.status(201);
    return snapshotView(row);
  });

  app.get<{ Params: { id: string } }>(
    '/v1/snapshots/:id',
    { config: { auth: 'client-or-agent' } },
    async (req) => {
      const row = store.snapshotById(req.params.id);
      if (!row) throw new GridstatorError('NOT_FOUND', `Snapshot ${req.params.id} not found`);
      return snapshotView(row);
    },
  );

  // ---- tokens ------------------------------------------------------------------------------

  app.post('/v1/tokens', { config: { auth: 'admin' } }, async (req, reply) => {
    const body = parse(CreateTokenRequestSchema, req.body, 'token');
    const ttl = body.ttlSeconds ?? (body.kind === 'join' ? 24 * 3600 : undefined);
    const issued = auth.issue({
      kind: body.kind,
      name: body.name,
      role: body.kind === 'client' ? body.role : null,
      expiresAt: ttl === undefined ? null : Date.now() + ttl * 1000,
      usesRemaining: body.kind === 'join' ? (body.maxUses ?? 1) : null,
    });
    reply.status(201);
    return { token: issued.token, info: tokenView(issued.row) };
  });

  app.get('/v1/tokens', { config: { auth: 'admin' } }, async () => ({
    tokens: store.listTokens().map(tokenView),
  }));

  app.delete<{ Params: { id: string } }>(
    '/v1/tokens/:id',
    { config: { auth: 'admin' } },
    async (req, reply) => {
      const row = store.tokenById(req.params.id);
      if (!row) throw new GridstatorError('NOT_FOUND', `Token ${req.params.id} not found`);
      if (row.id === principal(req).tokenId) {
        throw new GridstatorError('CONFLICT', 'Refusing to revoke the token used for this request');
      }
      store.revokeToken(row.id, Date.now());
      reply.status(204).send();
    },
  );

  // ---- client event stream -----------------------------------------------------------------

  app.get('/v1/events', { websocket: true }, (socket: WebSocket) => {
    const sub = hub.add(socket);
    socket.on('message', (raw) => {
      let data: unknown;
      try {
        data = JSON.parse(decodeWsData(raw));
      } catch {
        return hub.send(sub, { type: 'error', message: 'invalid JSON' });
      }
      const parsed = ClientEventMessageSchema.safeParse(data);
      if (!parsed.success) return hub.send(sub, { type: 'error', message: 'invalid message' });
      const msg = parsed.data;
      if (msg.type === 'subscribe') {
        for (const t of msg.topics) sub.topics.add(t);
        return;
      }
      if (msg.type === 'logs.unsubscribe') {
        sub.logs.delete(msg.jobId);
        return;
      }
      const job = store.jobById(msg.jobId);
      if (!job) return hub.send(sub, { type: 'error', message: `Job ${msg.jobId} not found` });
      // Backlog and live subscription are set up synchronously, so no entry can be missed.
      const backlog = logs.read(job.id, msg.afterSeq);
      for (let i = 0; i < backlog.length; i += 1000) {
        hub.send(sub, { type: 'logs', jobId: job.id, entries: backlog.slice(i, i + 1000) });
      }
      if (isTerminal(job.state))
        hub.send(sub, { type: 'logs.end', jobId: job.id, state: job.state });
      else sub.logs.add(job.id);
    });
  });

  return app as unknown as FastifyInstance;
}

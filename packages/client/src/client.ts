import type { Agent } from 'node:https';
import type { IncomingMessage } from 'node:http';
import type { Readable } from 'node:stream';
import { WebSocket } from 'ws';
import { z } from 'zod';
import { GridstatorError, GRIDSTATOR_VERSION, decodeWsData } from '@gridstator/shared';
import {
  ArtifactSchema,
  BlobsMissingResponseSchema,
  BlobsUploadResponseSchema,
  ClusterViewSchema,
  CreateTokenResponseSchema,
  HealthResponseSchema,
  JobEventSchema,
  JobSchema,
  ListJobsResponseSchema,
  ListNodesResponseSchema,
  LogsResponseSchema,
  NodeSchema,
  RegisterAgentResponseSchema,
  ServerEventMessageSchema,
  SnapshotViewSchema,
  TokenViewSchema,
  isTerminal,
  type Artifact,
  type BlobsUploadResponse,
  type ClientEventMessage,
  type ClusterView,
  type CreateTokenRequest,
  type CreateTokenResponse,
  type HealthResponse,
  type Job,
  type JobEvent,
  type JobSpecInput,
  type JobState,
  type LogEntry,
  type LogsResponse,
  type Node,
  type NodeSettingsPatch,
  type RegisterAgentRequest,
  type RegisterAgentResponse,
  type ServerEventMessage,
  type SnapshotView,
  type TokenView,
} from '@gridstator/protocol';
import {
  createPinnedAgent,
  errorFromResponse,
  networkError,
  pinnedTlsOptions,
  rawRequest,
  readBody,
} from './http.js';

export interface ClientOptions {
  /** `https://host:port` */
  url: string;
  /** PEM of the pinned cluster CA. */
  ca: string;
  /** Bearer token (client token, or agent credential for agent use). */
  token: string;
  userAgent?: string;
}

interface RequestOptions<T> {
  query?: Record<string, string | number | undefined>;
  json?: unknown;
  body?: Readable;
  contentType?: string;
  schema?: z.ZodType<T>;
  token?: string;
  timeoutMs?: number;
}

/** Typed client for the controller's v1 API. Shared by the CLI, agents and the VS Code extension. */
export class GridstatorClient {
  readonly url: string;
  private readonly ca: string;
  private readonly token: string;
  private readonly agent: Agent;
  private readonly userAgent: string;

  constructor(options: ClientOptions) {
    this.url = options.url.replace(/\/+$/, '');
    this.ca = options.ca;
    this.token = options.token;
    this.agent = createPinnedAgent({ ca: options.ca });
    this.userAgent = options.userAgent ?? `gridstator/${GRIDSTATOR_VERSION}`;
  }

  close(): void {
    this.agent.destroy();
  }

  private buildUrl(path: string, query?: Record<string, string | number | undefined>): string {
    const u = new URL(this.url + path);
    for (const [k, v] of Object.entries(query ?? {})) {
      if (v !== undefined) u.searchParams.set(k, String(v));
    }
    return u.toString();
  }

  private async send(
    method: string,
    path: string,
    opts: RequestOptions<unknown>,
  ): Promise<IncomingMessage> {
    const headers: Record<string, string> = {
      authorization: `Bearer ${opts.token ?? this.token}`,
      'user-agent': this.userAgent,
      accept: 'application/json',
    };
    let body: string | Readable | undefined;
    if (opts.json !== undefined) {
      headers['content-type'] = 'application/json';
      body = JSON.stringify(opts.json);
    } else if (opts.body) {
      headers['content-type'] = opts.contentType ?? 'application/octet-stream';
      body = opts.body;
    }
    let res: IncomingMessage;
    try {
      res = await rawRequest({
        method,
        url: this.buildUrl(path, opts.query),
        agent: this.agent,
        headers,
        ...(body === undefined ? {} : { body }),
        ...(opts.timeoutMs === undefined ? {} : { timeoutMs: opts.timeoutMs }),
      });
    } catch (err) {
      throw networkError(this.url, err);
    }
    if ((res.statusCode ?? 500) >= 400) throw await errorFromResponse(res);
    return res;
  }

  async request<T>(method: string, path: string, opts: RequestOptions<T> = {}): Promise<T> {
    const res = await this.send(method, path, opts);
    const text = (await readBody(res)).toString('utf8');
    if (res.statusCode === 204 || text === '') return undefined as T;
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new GridstatorError(
        'INTERNAL',
        `Controller returned invalid JSON for ${method} ${path}`,
      );
    }
    if (!opts.schema) return parsed as T;
    const result = opts.schema.safeParse(parsed);
    if (!result.success) {
      throw new GridstatorError(
        'INTERNAL',
        `Unexpected response from controller for ${method} ${path}`,
        {
          hint: 'The CLI and controller versions may be incompatible.',
          details: result.error.issues.slice(0, 5),
        },
      );
    }
    return result.data;
  }

  // ---- cluster & nodes ---------------------------------------------------------------------

  health(): Promise<HealthResponse> {
    return this.request('GET', '/v1/health', { schema: HealthResponseSchema, timeoutMs: 10_000 });
  }

  cluster(): Promise<ClusterView> {
    return this.request('GET', '/v1/cluster', { schema: ClusterViewSchema });
  }

  async listNodes(): Promise<Node[]> {
    return (await this.request('GET', '/v1/nodes', { schema: ListNodesResponseSchema })).nodes;
  }

  getNode(idOrName: string): Promise<Node> {
    return this.request('GET', `/v1/nodes/${encodeURIComponent(idOrName)}`, { schema: NodeSchema });
  }

  updateNode(idOrName: string, patch: NodeSettingsPatch): Promise<Node> {
    return this.request('PATCH', `/v1/nodes/${encodeURIComponent(idOrName)}`, {
      json: patch,
      schema: NodeSchema,
    });
  }

  async removeNode(idOrName: string): Promise<void> {
    await this.request('DELETE', `/v1/nodes/${encodeURIComponent(idOrName)}`);
  }

  registerAgent(joinToken: string, req: RegisterAgentRequest): Promise<RegisterAgentResponse> {
    return this.request('POST', '/v1/agents/register', {
      json: req,
      token: joinToken,
      schema: RegisterAgentResponseSchema,
    });
  }

  // ---- jobs --------------------------------------------------------------------------------

  submitJob(spec: JobSpecInput): Promise<Job> {
    return this.request('POST', '/v1/jobs', { json: { spec }, schema: JobSchema });
  }

  async listJobs(
    query: { state?: JobState[]; node?: string; limit?: number; before?: string } = {},
  ): Promise<Job[]> {
    const res = await this.request('GET', '/v1/jobs', {
      query: {
        state: query.state?.join(','),
        node: query.node,
        limit: query.limit,
        before: query.before,
      },
      schema: ListJobsResponseSchema,
    });
    return res.jobs;
  }

  getJob(id: string): Promise<Job> {
    return this.request('GET', `/v1/jobs/${encodeURIComponent(id)}`, { schema: JobSchema });
  }

  cancelJob(id: string): Promise<Job> {
    return this.request('POST', `/v1/jobs/${encodeURIComponent(id)}/cancel`, { schema: JobSchema });
  }

  retryJob(id: string): Promise<Job> {
    return this.request('POST', `/v1/jobs/${encodeURIComponent(id)}/retry`, { schema: JobSchema });
  }

  jobLogs(id: string, after = 0, limit?: number): Promise<LogsResponse> {
    return this.request('GET', `/v1/jobs/${encodeURIComponent(id)}/logs`, {
      query: { after, limit },
      schema: LogsResponseSchema,
    });
  }

  async jobEvents(id: string): Promise<JobEvent[]> {
    const res = await this.request('GET', `/v1/jobs/${encodeURIComponent(id)}/events`, {
      schema: z.object({ events: z.array(JobEventSchema) }),
    });
    return res.events;
  }

  async listArtifacts(id: string): Promise<Artifact[]> {
    const res = await this.request('GET', `/v1/jobs/${encodeURIComponent(id)}/artifacts`, {
      schema: z.object({ artifacts: z.array(ArtifactSchema) }),
    });
    return res.artifacts;
  }

  /** Streams one artifact's content. */
  downloadArtifact(id: string, name: string): Promise<IncomingMessage> {
    const encoded = name.split('/').map(encodeURIComponent).join('/');
    return this.send('GET', `/v1/jobs/${encodeURIComponent(id)}/artifacts/${encoded}`, {});
  }

  // ---- blobs & snapshots -------------------------------------------------------------------

  async missingBlobs(digests: string[]): Promise<string[]> {
    const res = await this.request('POST', '/v1/blobs/missing', {
      json: { digests },
      schema: BlobsMissingResponseSchema,
    });
    return res.missing;
  }

  uploadBlobs(body: Readable): Promise<BlobsUploadResponse> {
    return this.request('POST', '/v1/blobs/upload', {
      body,
      contentType: 'application/x-tar',
      schema: BlobsUploadResponseSchema,
    });
  }

  /** Returns a tar stream of the requested blobs (entries named sha256/<hex>). */
  async downloadBlobs(digests: string[]): Promise<IncomingMessage> {
    return this.send('POST', '/v1/blobs/download', { json: { digests } });
  }

  async getBlob(digest: string): Promise<Buffer> {
    const res = await this.send('GET', `/v1/blobs/${encodeURIComponent(digest)}`, {});
    return readBody(res, 512 * 1024 * 1024);
  }

  createSnapshot(manifestDigest: string): Promise<SnapshotView> {
    return this.request('POST', '/v1/snapshots', {
      json: { manifestDigest },
      schema: SnapshotViewSchema,
    });
  }

  getSnapshot(id: string): Promise<SnapshotView> {
    return this.request('GET', `/v1/snapshots/${encodeURIComponent(id)}`, {
      schema: SnapshotViewSchema,
    });
  }

  // ---- tokens ------------------------------------------------------------------------------

  createToken(req: CreateTokenRequest): Promise<CreateTokenResponse> {
    return this.request('POST', '/v1/tokens', { json: req, schema: CreateTokenResponseSchema });
  }

  async listTokens(): Promise<TokenView[]> {
    const res = await this.request('GET', '/v1/tokens', {
      schema: z.object({ tokens: z.array(TokenViewSchema) }),
    });
    return res.tokens;
  }

  async revokeToken(id: string): Promise<void> {
    await this.request('DELETE', `/v1/tokens/${encodeURIComponent(id)}`);
  }

  // ---- websockets --------------------------------------------------------------------------

  /** Opens a raw authenticated WebSocket to `path` (e.g. /v1/events or /v1/agent/ws). */
  openWebSocket(path: string): WebSocket {
    const wsUrl = this.url.replace(/^https:/, 'wss:') + path;
    return new WebSocket(wsUrl, {
      ...pinnedTlsOptions({ ca: this.ca }),
      headers: { authorization: `Bearer ${this.token}`, 'user-agent': this.userAgent },
      handshakeTimeout: 10_000,
      maxPayload: 64 * 1024 * 1024,
    });
  }

  /** Opens the client event stream. */
  events(): EventStream {
    return new EventStream(this.openWebSocket('/v1/events'), this.url);
  }

  /**
   * Streams a job's logs until the job is terminal and every entry has been delivered. Reconnects
   * transparently and never delivers an entry twice. Resolves with the job's final state.
   */
  async followLogs(
    jobId: string,
    onEntries: (entries: LogEntry[]) => void,
    options: { afterSeq?: number; signal?: AbortSignal; onReconnect?: (err: Error) => void } = {},
  ): Promise<JobState> {
    let last = options.afterSeq ?? 0;
    let failures = 0;
    for (;;) {
      if (options.signal?.aborted) throw new GridstatorError('CONFLICT', 'Log streaming aborted');
      try {
        return await new Promise<JobState>((resolve, reject) => {
          const stream = this.events();
          const abort = () => {
            stream.close();
            reject(new GridstatorError('CONFLICT', 'Log streaming aborted'));
          };
          options.signal?.addEventListener('abort', abort, { once: true });
          stream.on('open', () => {
            failures = 0;
            stream.send({ type: 'logs.subscribe', jobId, afterSeq: last });
          });
          stream.on('message', (msg: ServerEventMessage) => {
            if (msg.type === 'logs' && msg.jobId === jobId) {
              const fresh = msg.entries.filter((e) => e.s > last);
              if (fresh.length > 0) {
                last = fresh[fresh.length - 1]!.s;
                onEntries(fresh);
              }
            } else if (msg.type === 'logs.end' && msg.jobId === jobId) {
              options.signal?.removeEventListener('abort', abort);
              stream.close();
              resolve(msg.state);
            } else if (msg.type === 'error') {
              options.signal?.removeEventListener('abort', abort);
              stream.close();
              reject(new GridstatorError('NOT_FOUND', msg.message));
            }
          });
          stream.on('close', (err?: Error) => {
            options.signal?.removeEventListener('abort', abort);
            reject(err ?? new GridstatorError('UNAVAILABLE', 'Event stream closed'));
          });
        });
      } catch (err) {
        if (
          err instanceof GridstatorError &&
          (err.code === 'NOT_FOUND' ||
            err.code === 'CONFLICT' ||
            err.code === 'UNAUTHORIZED' ||
            err.code === 'TLS_PIN_MISMATCH')
        ) {
          throw err;
        }
        failures++;
        options.onReconnect?.(err as Error);
        if (failures > 30) throw err;
        await new Promise((r) => setTimeout(r, Math.min(5000, 250 * 2 ** Math.min(failures, 5))));
        // If the job finished while we were disconnected, the subscription replays the rest.
      }
    }
  }

  /** Polls until the job is terminal (used when streaming is not wanted). */
  async waitForJob(jobId: string, intervalMs = 1000): Promise<Job> {
    for (;;) {
      const job = await this.getJob(jobId);
      if (isTerminal(job.state)) return job;
      await new Promise((r) => setTimeout(r, intervalMs));
    }
  }
}

/** Thin typed wrapper over the /v1/events WebSocket. */
export class EventStream {
  private readonly ws: WebSocket;
  private readonly listeners = {
    open: new Set<() => void>(),
    message: new Set<(msg: ServerEventMessage) => void>(),
    close: new Set<(err?: Error) => void>(),
  };
  private closedError: Error | undefined;

  constructor(ws: WebSocket, baseUrl: string) {
    this.ws = ws;
    ws.on('open', () => this.listeners.open.forEach((l) => l()));
    ws.on('message', (data) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(decodeWsData(data));
      } catch {
        return;
      }
      const result = ServerEventMessageSchema.safeParse(parsed);
      if (result.success) this.listeners.message.forEach((l) => l(result.data));
    });
    ws.on('unexpected-response', (_req, res) => {
      void errorFromResponse(res).then((err) => {
        this.closedError = err;
        ws.terminate();
      });
    });
    ws.on('error', (err) => {
      this.closedError ??= networkError(baseUrl, err);
    });
    ws.on('close', () => this.listeners.close.forEach((l) => l(this.closedError)));
  }

  on(event: 'open', listener: () => void): this;
  on(event: 'message', listener: (msg: ServerEventMessage) => void): this;
  on(event: 'close', listener: (err?: Error) => void): this;
  on(event: 'open' | 'message' | 'close', listener: (...args: never[]) => void): this {
    (this.listeners[event] as Set<(...args: never[]) => void>).add(listener);
    return this;
  }

  send(msg: ClientEventMessage): void {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  close(): void {
    this.ws.close();
  }
}

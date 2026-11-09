import * as vscode from 'vscode';
import { GRIDSTATOR_VERSION, GridstatorError, errorMessage } from '@gridstator/shared';
import { isTerminal, type Job, type JobGroup, type Node } from '@gridstator/protocol';
import { GridstatorClient, resolveConnection, type EventStream } from '@gridstator/client';
import type { ConnectionInfo } from './messages';

const DISCONNECTED: ConnectionInfo = {
  status: 'signedOut',
  url: null,
  user: null,
  role: null,
  clusterId: null,
  version: null,
  error: null,
};

/**
 * Owns the connection to the controller and a live copy of nodes and recent jobs, kept current by
 * the /v1/events stream. Reconnects with backoff and resynchronises after every reconnect.
 */
export class ClusterState implements vscode.Disposable {
  connection: ConnectionInfo = { ...DISCONNECTED };
  client: GridstatorClient | undefined;
  readonly nodes = new Map<string, Node>();
  readonly jobs = new Map<string, Job>();
  /** Jobs started from this window. */
  readonly myJobs = new Set<string>();
  readonly groups = new Map<string, JobGroup>();
  /** Groups started from this window. */
  readonly myGroups = new Set<string>();

  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changed.event;
  private readonly finished = new vscode.EventEmitter<Job>();
  /** Fires once when a job reaches a terminal state while we are watching. */
  readonly onJobFinished = this.finished.event;
  private readonly groupFinished = new vscode.EventEmitter<JobGroup>();
  /** Fires once when a job group reaches a terminal state while we are watching. */
  readonly onGroupFinished = this.groupFinished.event;

  private events: EventStream | undefined;
  private retryTimer: NodeJS.Timeout | undefined;
  private attempt = 0;
  private generation = 0;
  private disposed = false;

  constructor(
    private readonly configPath: () => string | undefined,
    private readonly historyLimit: () => number,
    private readonly log: vscode.OutputChannel,
  ) {}

  get connected(): boolean {
    return this.connection.status === 'connected' && this.client !== undefined;
  }

  /** Returns the client or throws a user-facing error. */
  requireClient(): GridstatorClient {
    if (!this.client || this.connection.status !== 'connected') {
      throw new GridstatorError('UNAVAILABLE', 'Not connected to a Gridstator controller', {
        hint: 'Use "Gridstator: Sign In to Controller" or check the controller is running.',
      });
    }
    return this.client;
  }

  jobList(): Job[] {
    return [...this.jobs.values()].sort((a, b) => (a.id < b.id ? 1 : -1));
  }

  nodeList(): Node[] {
    return [...this.nodes.values()].sort((a, b) => a.name.localeCompare(b.name));
  }

  private setConnection(c: Partial<ConnectionInfo>): void {
    this.connection = { ...this.connection, ...c };
    void vscode.commands.executeCommand(
      'setContext',
      'gridstator.connection',
      this.connection.status,
    );
    void vscode.commands.executeCommand('setContext', 'gridstator.nodeCount', this.nodes.size);
    this.changed.fire();
  }

  /** (Re)connects from the client configuration. Safe to call repeatedly. */
  async connect(): Promise<void> {
    const gen = ++this.generation;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.events?.close();
    this.events = undefined;
    this.client?.close();
    this.client = undefined;
    let conn;
    try {
      conn = await resolveConnection(this.configPath() || undefined);
    } catch (err) {
      this.nodes.clear();
      this.jobs.clear();
      this.setConnection({ ...DISCONNECTED, status: 'signedOut', error: errorMessage(err) });
      return;
    }
    this.setConnection({ status: 'connecting', url: conn.url, error: null });
    const client = new GridstatorClient({
      ...conn,
      userAgent: `gridstator-vscode/${GRIDSTATOR_VERSION}`,
    });
    try {
      const [cluster, nodes, jobs, groups] = await Promise.all([
        client.cluster(),
        client.listNodes(),
        client.listJobs({ limit: this.historyLimit() }),
        client.listGroups(30),
      ]);
      if (gen !== this.generation || this.disposed) {
        client.close();
        return;
      }
      this.client = client;
      this.nodes.clear();
      for (const n of nodes) this.nodes.set(n.id, n);
      const previous = new Map(this.jobs);
      this.jobs.clear();
      for (const g of groups) this.upsertGroup(g, false);
      for (const j of jobs) {
        this.jobs.set(j.id, j);
        const before = previous.get(j.id);
        if (before && !isTerminal(before.state) && isTerminal(j.state)) this.finished.fire(j);
      }
      // Keep tracking our own jobs even if they fell out of the recent window.
      for (const id of this.myJobs) {
        const before = previous.get(id);
        if (!this.jobs.has(id) && before) this.jobs.set(id, before);
      }
      this.attempt = 0;
      this.setConnection({
        status: 'connected',
        user: cluster.whoami.name,
        role: cluster.whoami.role,
        clusterId: cluster.clusterId,
        version: cluster.version,
        error: null,
      });
      this.log.appendLine(
        `[${new Date().toISOString()}] Connected to ${conn.url} as ${cluster.whoami.name}`,
      );
      this.subscribe(client, gen);
    } catch (err) {
      client.close();
      if (gen !== this.generation || this.disposed) return;
      const message =
        err instanceof GridstatorError && err.hint
          ? `${err.message}. ${err.hint}`
          : errorMessage(err);
      this.log.appendLine(`[${new Date().toISOString()}] Connection failed: ${message}`);
      const fatal =
        err instanceof GridstatorError &&
        (err.code === 'UNAUTHORIZED' || err.code === 'TLS_PIN_MISMATCH');
      this.setConnection({ status: 'error', error: message });
      if (!fatal) this.scheduleReconnect(gen);
    }
  }

  private scheduleReconnect(gen: number): void {
    if (this.disposed) return;
    const delay = Math.min(30_000, 1000 * 2 ** Math.min(this.attempt++, 5));
    this.retryTimer = setTimeout(() => {
      if (gen === this.generation) void this.connect();
    }, delay);
  }

  private subscribe(client: GridstatorClient, gen: number): void {
    const stream = client.events();
    this.events = stream;
    stream.on('open', () => stream.send({ type: 'subscribe', topics: ['jobs', 'nodes'] }));
    stream.on('message', (msg) => {
      if (gen !== this.generation) return;
      switch (msg.type) {
        case 'job.updated':
          this.upsertJob(msg.job);
          break;
        case 'node.updated':
          this.nodes.set(msg.node.id, msg.node);
          this.changed.fire();
          void vscode.commands.executeCommand(
            'setContext',
            'gridstator.nodeCount',
            this.nodes.size,
          );
          break;
        case 'group.updated':
          this.upsertGroup(msg.group);
          break;
        case 'node.removed':
          this.nodes.delete(msg.nodeId);
          this.changed.fire();
          break;
        default:
          break;
      }
    });
    stream.on('close', (err) => {
      if (gen !== this.generation || this.disposed) return;
      this.log.appendLine(
        `[${new Date().toISOString()}] Event stream closed${err ? `: ${err.message}` : ''}; reconnecting`,
      );
      this.setConnection({ status: 'connecting', error: err?.message ?? null });
      this.scheduleReconnect(gen);
    });
  }

  upsertJob(job: Job): void {
    const before = this.jobs.get(job.id);
    this.jobs.set(job.id, job);
    if (this.jobs.size > this.historyLimit() + 50) {
      for (const j of this.jobList().slice(this.historyLimit())) {
        if (!this.myJobs.has(j.id) && isTerminal(j.state)) this.jobs.delete(j.id);
      }
    }
    if ((!before || !isTerminal(before.state)) && isTerminal(job.state)) this.finished.fire(job);
    this.changed.fire();
  }

  groupList(): JobGroup[] {
    return [...this.groups.values()].sort((a, b) => (a.id < b.id ? 1 : -1));
  }

  upsertGroup(group: JobGroup, notify = true): void {
    const before = this.groups.get(group.id);
    this.groups.set(group.id, group);
    const done = (g: JobGroup | undefined) =>
      g !== undefined && g.state !== 'queued' && g.state !== 'running';
    if (notify && !done(before) && done(group)) this.groupFinished.fire(group);
    if (this.groups.size > 60) {
      for (const g of this.groupList().slice(50))
        if (!this.myGroups.has(g.id)) this.groups.delete(g.id);
    }
    if (notify) this.changed.fire();
  }

  upsertNode(node: Node): void {
    this.nodes.set(node.id, node);
    this.changed.fire();
  }

  async refreshJob(id: string): Promise<Job | undefined> {
    if (!this.client) return undefined;
    const job = await this.client.getJob(id);
    this.upsertJob(job);
    return job;
  }

  dispose(): void {
    this.disposed = true;
    this.generation++;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.events?.close();
    this.client?.close();
    this.changed.dispose();
    this.finished.dispose();
    this.groupFinished.dispose();
  }
}

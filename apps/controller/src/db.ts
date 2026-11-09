import { DatabaseSync, type SQLInputValue } from 'node:sqlite';
import type { JobSpec, JobState, NodeInfo, NodeSettings, TokenRole } from '@gridstator/protocol';

const SCHEMA_VERSION = 1;

const MIGRATIONS: Record<number, string> = {
  1: `
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE tokens (
      id TEXT PRIMARY KEY,
      kind TEXT NOT NULL CHECK (kind IN ('client','join','agent')),
      name TEXT NOT NULL,
      hash TEXT NOT NULL UNIQUE,
      role TEXT,
      node_id TEXT,
      created_at INTEGER NOT NULL,
      expires_at INTEGER,
      uses_remaining INTEGER,
      last_used_at INTEGER,
      revoked_at INTEGER
    );
    CREATE TABLE nodes (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL UNIQUE,
      info TEXT,
      settings TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      last_seen_at INTEGER
    );
    CREATE TABLE jobs (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      kind TEXT NOT NULL,
      spec TEXT NOT NULL,
      state TEXT NOT NULL,
      state_reason TEXT,
      priority INTEGER NOT NULL,
      owner TEXT NOT NULL,
      snapshot_id TEXT,
      project TEXT,
      node_id TEXT,
      node_name TEXT,
      assignment_id TEXT,
      attempt INTEGER NOT NULL,
      retry_of TEXT,
      retries_left INTEGER NOT NULL,
      exit_code INTEGER,
      error TEXT,
      log_seq INTEGER NOT NULL DEFAULT 0,
      created_at INTEGER NOT NULL,
      queued_at INTEGER,
      assigned_at INTEGER,
      started_at INTEGER,
      finished_at INTEGER,
      cancel_requested_at INTEGER,
      updated_at INTEGER NOT NULL
    );
    CREATE INDEX jobs_state ON jobs(state);
    CREATE INDEX jobs_node_state ON jobs(node_id, state);
    CREATE INDEX jobs_project ON jobs(project, finished_at);
    CREATE TABLE job_events (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      job_id TEXT NOT NULL,
      ts INTEGER NOT NULL,
      type TEXT NOT NULL,
      from_state TEXT,
      to_state TEXT,
      actor TEXT NOT NULL,
      message TEXT
    );
    CREATE INDEX job_events_job ON job_events(job_id, id);
    CREATE TABLE snapshots (
      id TEXT PRIMARY KEY,
      file_count INTEGER NOT NULL,
      total_bytes INTEGER NOT NULL,
      owner TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      last_used_at INTEGER NOT NULL
    );
  `,
};

export interface TokenRow {
  id: string;
  kind: 'client' | 'join' | 'agent';
  name: string;
  hash: string;
  role: TokenRole | null;
  node_id: string | null;
  created_at: number;
  expires_at: number | null;
  uses_remaining: number | null;
  last_used_at: number | null;
  revoked_at: number | null;
}

export interface NodeRow {
  id: string;
  name: string;
  info: NodeInfo | null;
  settings: NodeSettings;
  created_at: number;
  last_seen_at: number | null;
}

export interface JobRow {
  id: string;
  name: string;
  kind: string;
  spec: JobSpec;
  state: JobState;
  state_reason: string | null;
  priority: number;
  owner: string;
  snapshot_id: string | null;
  project: string | null;
  node_id: string | null;
  node_name: string | null;
  assignment_id: string | null;
  attempt: number;
  retry_of: string | null;
  retries_left: number;
  exit_code: number | null;
  error: string | null;
  log_seq: number;
  created_at: number;
  queued_at: number | null;
  assigned_at: number | null;
  started_at: number | null;
  finished_at: number | null;
  cancel_requested_at: number | null;
  updated_at: number;
}

export interface JobEventRow {
  id: number;
  job_id: string;
  ts: number;
  type: string;
  from_state: JobState | null;
  to_state: JobState | null;
  actor: string;
  message: string | null;
}

export interface SnapshotRow {
  id: string;
  file_count: number;
  total_bytes: number;
  owner: string;
  created_at: number;
  last_used_at: number;
}

type Row = Record<string, SQLInputValue>;

function parseJob(row: Row): JobRow {
  return { ...(row as unknown as JobRow), spec: JSON.parse(row['spec'] as string) as JobSpec };
}

function parseNode(row: Row): NodeRow {
  return {
    ...(row as unknown as NodeRow),
    info: row['info'] ? (JSON.parse(row['info'] as string) as NodeInfo) : null,
    settings: JSON.parse(row['settings'] as string) as NodeSettings,
  };
}

const JOB_COLUMNS = [
  'id',
  'name',
  'kind',
  'spec',
  'state',
  'state_reason',
  'priority',
  'owner',
  'snapshot_id',
  'project',
  'node_id',
  'node_name',
  'assignment_id',
  'attempt',
  'retry_of',
  'retries_left',
  'exit_code',
  'error',
  'log_seq',
  'created_at',
  'queued_at',
  'assigned_at',
  'started_at',
  'finished_at',
  'cancel_requested_at',
  'updated_at',
] as const;

/**
 * Thin typed persistence layer over node:sqlite (ADR-8). All methods are synchronous, which keeps
 * state transitions atomic within the controller's single event loop.
 */
export class Store {
  readonly db: DatabaseSync;
  private depth = 0;

  constructor(path: string) {
    this.db = new DatabaseSync(path);
    this.db.exec(
      'PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;',
    );
    this.migrate();
  }

  close(): void {
    this.db.close();
  }

  private migrate(): void {
    this.db.exec('CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)');
    const row = this.db.prepare('SELECT version FROM schema_version').get() as
      { version: number } | undefined;
    let current = row?.version ?? 0;
    if (current > SCHEMA_VERSION) {
      throw new Error(
        `Database schema v${current} is newer than this controller (v${SCHEMA_VERSION}). Upgrade Gridstator.`,
      );
    }
    while (current < SCHEMA_VERSION) {
      const next = current + 1;
      this.tx(() => {
        this.db.exec(MIGRATIONS[next]!);
        this.db.exec('DELETE FROM schema_version');
        this.db.prepare('INSERT INTO schema_version (version) VALUES (?)').run(next);
      });
      current = next;
    }
  }

  /** Runs `fn` in a transaction (nested calls join the outer transaction). */
  tx<T>(fn: () => T): T {
    if (this.depth > 0) return fn();
    this.db.exec('BEGIN IMMEDIATE');
    this.depth++;
    try {
      const result = fn();
      this.db.exec('COMMIT');
      return result;
    } catch (err) {
      this.db.exec('ROLLBACK');
      throw err;
    } finally {
      this.depth--;
    }
  }

  // ---- meta ----------------------------------------------------------------------------------

  getMeta(key: string): string | undefined {
    const row = this.db.prepare('SELECT value FROM meta WHERE key = ?').get(key) as
      { value: string } | undefined;
    return row?.value;
  }

  setMeta(key: string, value: string): void {
    this.db
      .prepare(
        'INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
      )
      .run(key, value);
  }

  // ---- tokens --------------------------------------------------------------------------------

  insertToken(t: TokenRow): void {
    this.db
      .prepare(
        `INSERT INTO tokens (id, kind, name, hash, role, node_id, created_at, expires_at, uses_remaining, last_used_at, revoked_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        t.id,
        t.kind,
        t.name,
        t.hash,
        t.role,
        t.node_id,
        t.created_at,
        t.expires_at,
        t.uses_remaining,
        t.last_used_at,
        t.revoked_at,
      );
  }

  tokenByHash(hash: string): TokenRow | undefined {
    return this.db.prepare('SELECT * FROM tokens WHERE hash = ?').get(hash) as TokenRow | undefined;
  }

  tokenById(id: string): TokenRow | undefined {
    return this.db.prepare('SELECT * FROM tokens WHERE id = ?').get(id) as TokenRow | undefined;
  }

  listTokens(): TokenRow[] {
    return this.db
      .prepare('SELECT * FROM tokens ORDER BY created_at')
      .all() as unknown as TokenRow[];
  }

  touchToken(id: string, now: number): void {
    this.db.prepare('UPDATE tokens SET last_used_at = ? WHERE id = ?').run(now, id);
  }

  consumeJoinToken(id: string): void {
    this.db
      .prepare(
        'UPDATE tokens SET uses_remaining = uses_remaining - 1 WHERE id = ? AND uses_remaining > 0',
      )
      .run(id);
  }

  revokeToken(id: string, now: number): void {
    this.db
      .prepare('UPDATE tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL')
      .run(now, id);
  }

  revokeNodeTokens(nodeId: string, now: number): void {
    this.db
      .prepare('UPDATE tokens SET revoked_at = ? WHERE node_id = ? AND revoked_at IS NULL')
      .run(now, nodeId);
  }

  // ---- nodes ---------------------------------------------------------------------------------

  insertNode(n: NodeRow): void {
    this.db
      .prepare(
        'INSERT INTO nodes (id, name, info, settings, created_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?)',
      )
      .run(
        n.id,
        n.name,
        n.info ? JSON.stringify(n.info) : null,
        JSON.stringify(n.settings),
        n.created_at,
        n.last_seen_at,
      );
  }

  updateNode(
    id: string,
    patch: { info?: NodeInfo; settings?: NodeSettings; last_seen_at?: number },
  ): void {
    if (patch.info !== undefined)
      this.db.prepare('UPDATE nodes SET info = ? WHERE id = ?').run(JSON.stringify(patch.info), id);
    if (patch.settings !== undefined) {
      this.db
        .prepare('UPDATE nodes SET settings = ? WHERE id = ?')
        .run(JSON.stringify(patch.settings), id);
    }
    if (patch.last_seen_at !== undefined)
      this.db.prepare('UPDATE nodes SET last_seen_at = ? WHERE id = ?').run(patch.last_seen_at, id);
  }

  nodeById(id: string): NodeRow | undefined {
    const row = this.db.prepare('SELECT * FROM nodes WHERE id = ?').get(id) as Row | undefined;
    return row ? parseNode(row) : undefined;
  }

  nodeByName(name: string): NodeRow | undefined {
    const row = this.db.prepare('SELECT * FROM nodes WHERE name = ?').get(name) as Row | undefined;
    return row ? parseNode(row) : undefined;
  }

  listNodes(): NodeRow[] {
    return (this.db.prepare('SELECT * FROM nodes ORDER BY name').all() as Row[]).map(parseNode);
  }

  deleteNode(id: string): void {
    this.db.prepare('DELETE FROM nodes WHERE id = ?').run(id);
  }

  // ---- jobs ----------------------------------------------------------------------------------

  insertJob(j: JobRow): void {
    const values = JOB_COLUMNS.map((c) =>
      c === 'spec' ? JSON.stringify(j.spec) : (j[c] as SQLInputValue),
    );
    this.db
      .prepare(
        `INSERT INTO jobs (${JOB_COLUMNS.join(', ')}) VALUES (${JOB_COLUMNS.map(() => '?').join(', ')})`,
      )
      .run(...values);
  }

  /** Updates the given columns of a job. */
  updateJob(id: string, patch: Partial<Omit<JobRow, 'id' | 'spec'>>): void {
    const keys = Object.keys(patch) as Array<keyof typeof patch>;
    if (keys.length === 0) return;
    const sets = keys.map((k) => `${k} = ?`).join(', ');
    this.db
      .prepare(`UPDATE jobs SET ${sets} WHERE id = ?`)
      .run(...keys.map((k) => patch[k] as SQLInputValue), id);
  }

  jobById(id: string): JobRow | undefined {
    const row = this.db.prepare('SELECT * FROM jobs WHERE id = ?').get(id) as Row | undefined;
    return row ? parseJob(row) : undefined;
  }

  jobsInStates(states: readonly JobState[]): JobRow[] {
    const rows = this.db
      .prepare(
        `SELECT * FROM jobs WHERE state IN (${states.map(() => '?').join(', ')}) ORDER BY id`,
      )
      .all(...states) as Row[];
    return rows.map(parseJob);
  }

  jobsOnNode(nodeId: string, states: readonly JobState[]): JobRow[] {
    const rows = this.db
      .prepare(
        `SELECT * FROM jobs WHERE node_id = ? AND state IN (${states.map(() => '?').join(', ')}) ORDER BY id`,
      )
      .all(nodeId, ...states) as Row[];
    return rows.map(parseJob);
  }

  listJobs(q: {
    states?: readonly JobState[];
    nodeId?: string;
    limit: number;
    before?: string;
  }): JobRow[] {
    const where: string[] = [];
    const params: SQLInputValue[] = [];
    if (q.states && q.states.length > 0) {
      where.push(`state IN (${q.states.map(() => '?').join(', ')})`);
      params.push(...q.states);
    }
    if (q.nodeId) {
      where.push('node_id = ?');
      params.push(q.nodeId);
    }
    if (q.before) {
      where.push('id < ?');
      params.push(q.before);
    }
    const sql = `SELECT * FROM jobs ${where.length ? `WHERE ${where.join(' AND ')}` : ''} ORDER BY id DESC LIMIT ?`;
    return (this.db.prepare(sql).all(...params, q.limit) as Row[]).map(parseJob);
  }

  /** 1-based position among queued jobs (priority desc, queue time asc). */
  queuePosition(job: JobRow): number {
    const row = this.db
      .prepare(
        `SELECT COUNT(*) AS n FROM jobs WHERE state = 'queued' AND (
           priority > ? OR (priority = ? AND (queued_at < ? OR (queued_at = ? AND id < ?))))`,
      )
      .get(job.priority, job.priority, job.queued_at ?? 0, job.queued_at ?? 0, job.id) as {
      n: number;
    };
    return row.n + 1;
  }

  countJobsByState(): Record<string, number> {
    const rows = this.db
      .prepare('SELECT state, COUNT(*) AS n FROM jobs GROUP BY state')
      .all() as Array<{ state: string; n: number }>;
    return Object.fromEntries(rows.map((r) => [r.state, r.n]));
  }

  /** Nodes that finished a job for `project` recently (most recent first). */
  recentNodesForProject(project: string, since: number): string[] {
    const rows = this.db
      .prepare(
        `SELECT node_id, MAX(finished_at) AS last FROM jobs
         WHERE project = ? AND node_id IS NOT NULL AND finished_at >= ? AND state = 'succeeded'
         GROUP BY node_id ORDER BY last DESC LIMIT 3`,
      )
      .all(project, since) as Array<{ node_id: string }>;
    return rows.map((r) => r.node_id);
  }

  terminalJobsFinishedBefore(ts: number, limit: number): Array<{ id: string }> {
    return this.db
      .prepare(
        `SELECT id FROM jobs WHERE state IN ('succeeded','failed','cancelled','lost') AND finished_at < ? LIMIT ?`,
      )
      .all(ts, limit) as Array<{ id: string }>;
  }

  deleteJob(id: string): void {
    this.db.prepare('DELETE FROM job_events WHERE job_id = ?').run(id);
    this.db.prepare('DELETE FROM jobs WHERE id = ?').run(id);
  }

  // ---- job events ----------------------------------------------------------------------------

  insertEvent(e: Omit<JobEventRow, 'id'>): void {
    this.db
      .prepare(
        'INSERT INTO job_events (job_id, ts, type, from_state, to_state, actor, message) VALUES (?, ?, ?, ?, ?, ?, ?)',
      )
      .run(e.job_id, e.ts, e.type, e.from_state, e.to_state, e.actor, e.message);
  }

  eventsForJob(jobId: string): JobEventRow[] {
    return this.db
      .prepare('SELECT * FROM job_events WHERE job_id = ? ORDER BY id')
      .all(jobId) as unknown as JobEventRow[];
  }

  // ---- snapshots -----------------------------------------------------------------------------

  upsertSnapshot(s: SnapshotRow): SnapshotRow {
    this.db
      .prepare(
        `INSERT INTO snapshots (id, file_count, total_bytes, owner, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET last_used_at = excluded.last_used_at`,
      )
      .run(s.id, s.file_count, s.total_bytes, s.owner, s.created_at, s.last_used_at);
    return this.snapshotById(s.id)!;
  }

  snapshotById(id: string): SnapshotRow | undefined {
    return this.db.prepare('SELECT * FROM snapshots WHERE id = ?').get(id) as
      SnapshotRow | undefined;
  }

  touchSnapshot(id: string, now: number): void {
    this.db.prepare('UPDATE snapshots SET last_used_at = ? WHERE id = ?').run(now, id);
  }

  listSnapshotIds(): string[] {
    return (this.db.prepare('SELECT id FROM snapshots').all() as Array<{ id: string }>).map(
      (r) => r.id,
    );
  }

  /** Snapshots unused since `ts` and not referenced by any non-terminal job. */
  staleSnapshots(ts: number): string[] {
    return (
      this.db
        .prepare(
          `SELECT id FROM snapshots s WHERE last_used_at < ? AND NOT EXISTS (
             SELECT 1 FROM jobs j WHERE j.snapshot_id = s.id AND j.state NOT IN ('succeeded','failed','cancelled','lost'))`,
        )
        .all(ts) as Array<{ id: string }>
    ).map((r) => r.id);
  }

  deleteSnapshot(id: string): void {
    this.db.prepare('DELETE FROM snapshots WHERE id = ?').run(id);
  }
}

import { GridstatorError, formatDuration, newId, type Logger } from '@gridstator/shared';
import {
  inUpdateWindow,
  StartUpdateRequestSchema,
  TOOL_NAMES,
  toolPlatform,
  type ToolPlatform,
  type AgentBuild,
  type AgentUpdateCheckResponse,
  type AutoUpdateMode,
  type ClusterSettings,
  type NodeAutoUpdate,
  type NodeUpdateStatus,
  type Rollout,
  type RolloutNode,
  type StartUpdateRequest,
  type UpdatesView,
} from '@gridstator/protocol';
import type { BuildSource } from './build.js';
import type { Store } from './db.js';
import type { ToolchainMirror } from './toolchain.js';

/** What the update manager needs to know about a node. */
export interface UpdateNode {
  id: string;
  name: string;
  /** Connected and welcomed on the agent channel. */
  online: boolean;
  build: AgentBuild | undefined;
  agentVersion: string | undefined;
  autoUpdate: NodeAutoUpdate;
  /** Jobs assigned to, starting or running on the node. */
  activeJobs: number;
  platform: string | undefined;
  arch: string | undefined;
}

/** The cluster, as seen by the update manager. */
export interface UpdateHost {
  nodes(): UpdateNode[];
  node(idOrName: string): UpdateNode | undefined;
  /** Nudges an agent to call the update endpoint; false if it is not connected. */
  sendUpdateCheck(nodeId: string): boolean;
  cancelJobs(nodeId: string, reason: string): void;
  /** Node status changed (rollout progress, hold released, …). */
  changed(nodeId: string): void;
  requestSchedule(): void;
}

export interface UpdateTiming {
  /** Window the launcher gives a new version to reconnect before rolling it back. */
  confirmWithinSeconds: number;
  /** A node that does not come back on the target build within this long has failed. */
  updateTimeoutMs: number;
  /** How often to repeat the nudge while a node has not started updating. */
  recheckMs: number;
  /** Agents in update-only mode poll the endpoint; they count as connected for this long. */
  updateOnlyFreshMs: number;
}

export const DEFAULT_UPDATE_TIMING: UpdateTiming = {
  confirmWithinSeconds: 600,
  updateTimeoutMs: 15 * 60_000,
  recheckMs: 60_000,
  updateOnlyFreshMs: 3 * 60_000,
};

interface Entry extends RolloutNode {
  /** When the node must be back on the target build. */
  deadline?: number;
  /** Running jobs were cancelled (drain timed out, or --now). */
  cancelled?: boolean;
  /** When the agent was last nudged to check for the update. */
  nudgedAt?: number;
}

interface InternalRollout extends Omit<Rollout, 'nodes'> {
  nodes: Entry[];
}

interface Failure {
  target: string;
  error: string;
  at: number;
}

interface Persisted {
  version: 1;
  rollout: InternalRollout | null;
  failures: Record<string, Failure>;
}

const META_KEY = 'updates';
const ACTIVE: ReadonlyArray<RolloutNode['state']> = ['draining', 'updating'];

/**
 * Brings workers to the controller's build: one node at a time, draining it first (cancelling
 * jobs after the drain timeout, or straight away with `now`), then nudging its agent to download
 * the build. A node is done when it reconnects on the target build; a failed download, a launcher
 * rollback or a timeout fails the node and halts the rollout until an operator starts a new one.
 *
 * Auto-update adds behind nodes (mode `follow`, or `window` while inside the window) to a running
 * rollout or starts one. Nodes that fail are not retried automatically.
 */
export class UpdateManager {
  private readonly store: Store;
  private readonly build: BuildSource;
  private readonly settings: () => ClusterSettings;
  private readonly log: Logger;
  private readonly timing: UpdateTiming;
  private readonly now: () => number;
  private host: UpdateHost | undefined;
  private state: Persisted;
  /** Agents that called the endpoint in update-only mode (protocol mismatch). */
  private readonly updateOnly = new Map<string, { current: string; at: number }>();
  private readonly toolchain: ToolchainMirror | undefined;

  constructor(deps: {
    store: Store;
    build: BuildSource;
    settings: () => ClusterSettings;
    logger: Logger;
    timing?: Partial<UpdateTiming>;
    now?: () => number;
    toolchain?: ToolchainMirror;
  }) {
    this.store = deps.store;
    this.build = deps.build;
    this.toolchain = deps.toolchain;
    this.settings = deps.settings;
    this.log = deps.logger;
    this.timing = { ...DEFAULT_UPDATE_TIMING, ...deps.timing };
    this.now = deps.now ?? Date.now;
    this.state = this.load();
  }

  attach(host: UpdateHost): void {
    this.host = host;
  }

  private load(): Persisted {
    const empty: Persisted = { version: 1, rollout: null, failures: {} };
    const raw = this.store.getMeta(META_KEY);
    if (!raw) return empty;
    try {
      const parsed = JSON.parse(raw) as Persisted;
      // A rollout (or failure) for another build is moot once the controller runs a new one.
      if (parsed.rollout && parsed.rollout.target.id !== this.build.ref.id) parsed.rollout = null;
      for (const [id, f] of Object.entries(parsed.failures ?? {}))
        if (f.target !== this.build.ref.id) delete parsed.failures[id];
      return { version: 1, rollout: parsed.rollout ?? null, failures: parsed.failures ?? {} };
    } catch {
      return empty;
    }
  }

  private save(): void {
    this.store.setMeta(META_KEY, JSON.stringify(this.state));
  }

  private get target(): string {
    return this.build.ref.id;
  }

  // ---- queries ----------------------------------------------------------------------------

  effectiveMode(node: Pick<UpdateNode, 'autoUpdate'>): AutoUpdateMode {
    return node.autoUpdate === 'inherit' ? this.settings().autoUpdate : node.autoUpdate;
  }

  private inWindow(): boolean {
    const w = this.settings().updateWindow;
    return w !== null && inUpdateWindow(w, new Date(this.now()));
  }

  private activeEntry(nodeId?: string): Entry | undefined {
    const r = this.state.rollout;
    if (!r || r.state !== 'running') return undefined;
    return r.nodes.find((e) => ACTIVE.includes(e.state) && (!nodeId || e.nodeId === nodeId));
  }

  /** The scheduler places no new jobs on a node while it drains or restarts for an update. */
  isHeld(nodeId: string): boolean {
    return this.activeEntry(nodeId) !== undefined;
  }

  private updateOnlyFresh(nodeId: string): { current: string; at: number } | undefined {
    const u = this.updateOnly.get(nodeId);
    return u && this.now() - u.at < this.timing.updateOnlyFreshMs ? u : undefined;
  }

  private failure(nodeId: string): Failure | undefined {
    const f = this.state.failures[nodeId];
    return f && f.target === this.target ? f : undefined;
  }

  private platformOf(n: UpdateNode): ToolPlatform | null {
    return n.platform && n.arch ? toolPlatform(n.platform, n.arch) : null;
  }

  /**
   * How a self-updating node's act / Node.js differ from what the controller hands out, e.g.
   * "node 24.15.0 → 24.22.2". Tools the mirror does not have ready yet do not count.
   */
  toolGaps(n: UpdateNode): string[] {
    const platform = this.platformOf(n);
    if (!this.toolchain || !n.build?.managed || !platform) return [];
    const want = this.toolchain.expected(platform);
    const tools = n.build.tools;
    const gaps: string[] = [];
    for (const name of TOOL_NAMES) {
      const v = want[name];
      if (!v) continue;
      const managed = tools?.managed.includes(name) ?? false;
      const have = tools?.[name] ?? null;
      if (managed && have === v) continue;
      gaps.push(`${name} ${have ? `${have}${managed ? '' : ' (system)'}` : 'missing'} → ${v}`);
    }
    return gaps;
  }

  /** Runs the controller's build and, if self-updating, the toolchain it hands out. */
  isCurrent(n: UpdateNode): boolean {
    return n.build?.id === this.target && this.toolGaps(n).length === 0;
  }

  status(node: UpdateNode, lastSeenAt: number | null): NodeUpdateStatus {
    const autoUpdate = this.effectiveMode(node);
    const canUpdate = node.build?.managed === true;
    const base = { autoUpdate, canUpdate, build: node.build?.id ?? null };
    const active = this.activeEntry(node.id);
    if (active) {
      return {
        ...base,
        state: 'updating',
        detail:
          active.state === 'draining'
            ? `draining before updating to ${this.target} (${node.activeJobs} job(s) left)`
            : `restarting on ${this.target}`,
      };
    }
    if (!node.online) {
      const u = this.updateOnlyFresh(node.id);
      if (u) {
        return {
          ...base,
          build: u.current,
          canUpdate: true,
          state: 'behind',
          detail: `runs ${u.current}, which cannot join this controller; ${this.behindReason(node, true)}`,
        };
      }
      const seen = lastSeenAt ? ` since ${new Date(lastSeenAt).toISOString()}` : '';
      return {
        ...base,
        state: 'unknown',
        detail: node.build ? `offline${seen}; last reported ${node.build.id}` : `offline${seen}`,
      };
    }
    if (this.isCurrent(node)) return { ...base, state: 'current', detail: null };
    const failed = this.failure(node.id);
    if (failed) return { ...base, state: 'failed', detail: failed.error };
    const gaps = node.build?.id === this.target ? this.toolGaps(node) : [];
    const reason = this.behindReason(node, false);
    return {
      ...base,
      state: 'behind',
      detail: gaps.length > 0 ? `toolchain ${gaps.join(', ')}; ${reason}` : reason,
    };
  }

  private behindReason(node: UpdateNode, updateOnly: boolean): string {
    if (!updateOnly && !node.build)
      return `agent ${node.agentVersion ?? '?'} predates self-update; install the new version on it once`;
    if (!updateOnly && !node.build?.managed)
      return 'not a managed install; run `gridstator agent install-service --force` on it once';
    const ctl = this.build.view();
    if (!ctl.updatable) return ctl.reason ?? 'the controller cannot serve its build';
    const r = this.state.rollout;
    const entry = r?.nodes.find((e) => e.nodeId === node.id && e.state === 'pending');
    if (r?.state === 'halted' && entry) return `rollout halted: ${r.haltReason ?? 'unknown'}`;
    if (entry) return 'waiting for its turn in the rollout';
    const mode = this.effectiveMode(node);
    if (mode === 'off') return 'auto-update is off; run `gridstator agents update`';
    if (mode === 'window') {
      const w = this.settings().updateWindow;
      return w
        ? `waiting for the update window (${w.start}-${w.end})`
        : 'auto-update mode is window, but no update window is set';
    }
    return 'update pending';
  }

  view(): UpdatesView {
    const s = this.settings();
    return {
      controller: this.build.view(),
      autoUpdate: s.autoUpdate,
      updateWindow: s.updateWindow,
      rollout: this.state.rollout ? publicRollout(this.state.rollout) : null,
      ...(this.toolchain ? { toolchain: this.toolchain.view(this.platformsInUse()) } : {}),
    };
  }

  /** Platforms of self-updating nodes: the ones the mirror has to serve. */
  private platformsInUse(): ToolPlatform[] {
    const set = new Set<ToolPlatform>();
    for (const n of this.host?.nodes() ?? []) {
      const p = this.platformOf(n);
      if (p && n.build?.managed) set.add(p);
    }
    return [...set].sort();
  }

  // ---- operator actions -------------------------------------------------------------------

  start(input: StartUpdateRequest, actor: string): UpdatesView {
    const host = this.requireHost();
    const req = StartUpdateRequestSchema.parse(input);
    const ctl = this.build.view();
    if (!ctl.updatable)
      throw new GridstatorError('CONFLICT', `Workers cannot update: ${ctl.reason ?? 'unknown'}`);
    const selected = req.nodes
      ? req.nodes.map((name) => {
          const n = host.node(name);
          if (!n) throw new GridstatorError('NOT_FOUND', `No node with id or name "${name}"`);
          return n;
        })
      : host.nodes().filter((n) => n.online && !this.isCurrent(n));
    const wanted = selected.filter(
      (n) => !this.isCurrent(n) || (!n.online && this.updateOnlyFresh(n.id)),
    );
    for (const n of wanted) delete this.state.failures[n.id];
    const now = this.now();
    let r = this.state.rollout;
    if (!r || r.state !== 'running') {
      r = {
        id: newId(now),
        target: this.build.ref,
        state: 'running',
        haltReason: null,
        startedBy: actor,
        startedAt: now,
        finishedAt: null,
        now: req.now,
        drainTimeoutSeconds: req.drainTimeoutSeconds,
        nodes: [],
      };
      this.state.rollout = r;
    } else {
      r.now = r.now || req.now;
      r.drainTimeoutSeconds = req.drainTimeoutSeconds;
    }
    for (const n of wanted) {
      const existing = r.nodes.find((e) => e.nodeId === n.id);
      if (existing && ACTIVE.includes(existing.state)) continue;
      if (existing) {
        Object.assign(existing, pendingEntry(n, false));
      } else {
        r.nodes.push(pendingEntry(n, false));
      }
    }
    this.log.info(
      { actor, target: this.target, nodes: wanted.map((n) => n.name), now: req.now },
      'worker update started',
    );
    this.save();
    this.tick();
    return this.view();
  }

  /** Stops the running rollout; the node being updated right now finishes on its own. */
  cancel(actor: string): UpdatesView {
    const r = this.state.rollout;
    if (!r || r.state !== 'running') throw new GridstatorError('CONFLICT', 'No update is running');
    const active = this.activeEntry();
    this.halt(r, `stopped by ${actor}`);
    if (active) this.host?.changed(active.nodeId);
    this.host?.requestSchedule();
    return this.view();
  }

  // ---- agent events -----------------------------------------------------------------------

  /** Called by the update endpoint. */
  check(nodeId: string, current: string, mode: 'normal' | 'update-only'): AgentUpdateCheckResponse {
    if (mode === 'update-only') {
      const first = !this.updateOnlyFresh(nodeId);
      this.updateOnly.set(nodeId, { current, at: this.now() });
      if (first) {
        this.tick();
        this.host?.changed(nodeId);
      }
    }
    const pkg = this.build.package;
    if (!pkg) return { action: 'none', reason: this.build.view().reason };
    if (current === this.target) return { action: 'none', reason: 'already current' };
    const entry = this.activeEntry(nodeId);
    if (entry?.state !== 'updating') return { action: 'none', reason: 'not scheduled for update' };
    return {
      action: 'update',
      build: this.build.ref,
      package: { sha256: pkg.sha256, size: pkg.size },
      confirmWithinSeconds: this.timing.confirmWithinSeconds,
    };
  }

  /** Called when an agent reports that it could not install the update. */
  report(nodeId: string, target: string, error: string): void {
    if (target !== this.target) return;
    const name = this.host?.node(nodeId)?.name ?? nodeId;
    this.log.warn({ node: name, target, error }, 'worker update failed');
    this.state.failures[nodeId] = { target, error, at: this.now() };
    const entry = this.activeEntry(nodeId);
    if (entry) this.fail(entry, error);
    this.save();
    this.host?.changed(nodeId);
    this.host?.requestSchedule();
  }

  /** Called after an agent (re)joined over the agent channel. */
  agentJoined(nodeId: string): void {
    this.updateOnly.delete(nodeId);
    // Back on a new build but not done yet (e.g. its toolchain): nudge it again right away.
    const active = this.activeEntry(nodeId);
    if (active?.state === 'updating') delete active.nudgedAt;
    this.tick();
  }

  // ---- progress ---------------------------------------------------------------------------

  /** Advances the rollout; called periodically and after relevant events. */
  tick(): void {
    const host = this.host;
    if (!host) return;
    try {
      this.autoEnqueue(host);
      const r = this.state.rollout;
      if (!r || r.state !== 'running') return;
      let changed = false;
      // At most a few transitions per tick: finishing one node may start the next.
      for (let i = 0; i < 4 && r.state === 'running'; i++) {
        const step = this.advance(host, r);
        if (!step) break;
        changed = true;
      }
      if (changed) this.save();
    } catch (err) {
      this.log.error({ err }, 'update tick failed');
    }
  }

  /** Performs one transition; returns false when there is nothing to do right now. */
  private advance(host: UpdateHost, r: InternalRollout): boolean {
    const now = this.now();
    const active = r.nodes.find((e) => ACTIVE.includes(e.state));
    if (active) {
      const n = host.node(active.nodeId);
      if (!n) return this.finishEntry(active, 'skipped', 'node was removed');
      if (n.online && this.isCurrent(n)) return this.finishEntry(active, 'done', null);
      const rb = n.build?.lastRollback;
      if (
        n.online &&
        rb &&
        rb.from === this.target &&
        active.startedAt !== null &&
        rb.at >= active.startedAt
      ) {
        const why = rb.reason === 'crashed' ? 'kept crashing' : 'did not reconnect in time';
        return this.fail(active, `${this.target} ${why}; the launcher rolled back to ${rb.to}`);
      }
      if (active.state === 'draining') {
        if (!n.online) return this.finishEntry(active, 'skipped', 'went offline while draining');
        if (n.activeJobs === 0) {
          active.state = 'updating';
          active.detail = null;
          active.deadline = now + this.timing.updateTimeoutMs;
          active.nudgedAt = now;
          host.sendUpdateCheck(n.id);
          host.changed(n.id);
          return true;
        }
        const timedOut =
          active.startedAt !== null && now - active.startedAt > r.drainTimeoutSeconds * 1000;
        if ((r.now || timedOut) && !active.cancelled) {
          active.cancelled = true;
          active.detail = r.now
            ? 'cancelling running jobs (--now)'
            : 'drain timed out; cancelling running jobs';
          host.cancelJobs(n.id, `${n.name} is being updated to ${this.target}`);
          return true;
        }
        return false;
      }
      // updating
      if (active.deadline !== undefined && now > active.deadline) {
        return this.fail(
          active,
          `did not come back on ${this.target} within ${formatDuration(this.timing.updateTimeoutMs)}`,
        );
      }
      if (
        n.online &&
        (active.nudgedAt === undefined || now - active.nudgedAt > this.timing.recheckMs)
      ) {
        active.nudgedAt = now;
        host.sendUpdateCheck(n.id);
        return true;
      }
      return false;
    }
    const inWindow = this.inWindow();
    const next = r.nodes.find((e) => {
      if (e.state !== 'pending') return false;
      if (!e.auto) return true;
      const n = host.node(e.nodeId);
      return !n || this.effectiveMode(n) !== 'window' || inWindow;
    });
    if (!next) {
      if (r.nodes.some((e) => e.state === 'pending')) return false;
      r.state = 'finished';
      r.finishedAt = now;
      this.log.info({ target: this.target }, 'worker update finished');
      return true;
    }
    const n = host.node(next.nodeId);
    if (!n) return this.finishEntry(next, 'skipped', 'node was removed');
    if (n.online && this.isCurrent(n)) return this.finishEntry(next, 'done', 'already current');
    if (n.online) {
      if (!n.build?.managed)
        return this.finishEntry(
          next,
          'skipped',
          n.build ? 'not a managed install' : 'agent predates self-update',
        );
      next.state = 'draining';
      next.startedAt = now;
      next.detail = null;
      host.changed(n.id);
      host.requestSchedule();
      return true;
    }
    if (this.updateOnlyFresh(n.id)) {
      // Cannot run jobs anyway: nothing to drain. It picks the update up on its next poll.
      next.state = 'updating';
      next.startedAt = now;
      next.deadline = now + this.timing.updateTimeoutMs;
      host.changed(n.id);
      return true;
    }
    return this.finishEntry(next, 'skipped', 'offline');
  }

  private finishEntry(e: Entry, state: 'done' | 'skipped', detail: string | null): true {
    const wasActive = ACTIVE.includes(e.state);
    e.state = state;
    e.detail = detail;
    e.finishedAt = this.now();
    delete e.deadline;
    if (state === 'done') delete this.state.failures[e.nodeId];
    this.log.info({ node: e.name, state, detail }, 'worker update step finished');
    if (wasActive) {
      this.host?.changed(e.nodeId);
      this.host?.requestSchedule();
    }
    return true;
  }

  private fail(e: Entry, error: string): true {
    e.state = 'failed';
    e.detail = error;
    e.finishedAt = this.now();
    delete e.deadline;
    this.state.failures[e.nodeId] = { target: this.target, error, at: this.now() };
    const r = this.state.rollout;
    if (r && r.state === 'running') this.halt(r, `${e.name}: ${error}`);
    this.log.warn({ node: e.name, error }, 'worker update failed; rollout halted');
    this.host?.changed(e.nodeId);
    this.host?.requestSchedule();
    return true;
  }

  private halt(r: InternalRollout, reason: string): void {
    r.state = 'halted';
    r.haltReason = reason;
    r.finishedAt = this.now();
    for (const e of r.nodes) {
      if (e.state === 'draining') {
        e.state = 'pending';
        e.startedAt = null;
        delete e.cancelled;
      }
    }
    this.save();
  }

  private autoEnqueue(host: UpdateHost): void {
    if (!this.build.package) return;
    const r = this.state.rollout;
    // A halted rollout waits for an operator; so does everything else for this build.
    if (r?.state === 'halted') return;
    const inWindow = this.inWindow();
    let dirty = false;
    if (r?.state === 'running') {
      // Drop auto entries for nodes that turned auto-update off.
      const before = r.nodes.length;
      r.nodes = r.nodes.filter((e) => {
        if (!e.auto || e.state !== 'pending') return true;
        const n = host.node(e.nodeId);
        return n !== undefined && this.effectiveMode(n) !== 'off';
      });
      dirty = r.nodes.length !== before;
    }
    const candidates = host.nodes().filter((n) => {
      const mode = this.effectiveMode(n);
      if (mode === 'off' || (mode === 'window' && !inWindow)) return false;
      if (this.failure(n.id)) return false;
      if (n.online) return n.build?.managed === true && !this.isCurrent(n);
      return this.updateOnlyFresh(n.id) !== undefined;
    });
    const fresh = candidates.filter((n) => {
      const e = r?.state === 'running' ? r.nodes.find((x) => x.nodeId === n.id) : undefined;
      return !e || e.state === 'skipped' || e.state === 'done';
    });
    if (fresh.length > 0) {
      const now = this.now();
      let target = r;
      if (!target || target.state !== 'running') {
        target = {
          id: newId(now),
          target: this.build.ref,
          state: 'running',
          haltReason: null,
          startedBy: 'auto-update',
          startedAt: now,
          finishedAt: null,
          now: false,
          drainTimeoutSeconds: 3600,
          nodes: [],
        };
        this.state.rollout = target;
        this.log.info(
          { target: this.target, nodes: fresh.map((n) => n.name) },
          'auto-update started',
        );
      }
      for (const n of fresh) {
        const e = target.nodes.find((x) => x.nodeId === n.id);
        if (e) Object.assign(e, pendingEntry(n, true));
        else target.nodes.push(pendingEntry(n, true));
      }
      dirty = true;
    }
    if (dirty) this.save();
  }

  private requireHost(): UpdateHost {
    if (!this.host) throw new GridstatorError('INTERNAL', 'update manager is not attached');
    return this.host;
  }
}

function pendingEntry(n: UpdateNode, auto: boolean): Entry {
  return {
    nodeId: n.id,
    name: n.name,
    state: 'pending',
    detail: null,
    auto,
    startedAt: null,
    finishedAt: null,
  };
}

function publicRollout(r: InternalRollout): Rollout {
  return {
    ...r,
    target: { ...r.target },
    nodes: r.nodes.map((e) => ({
      nodeId: e.nodeId,
      name: e.name,
      state: e.state,
      detail: e.detail,
      auto: e.auto,
      startedAt: e.startedAt,
      finishedAt: e.finishedAt,
    })),
  };
}

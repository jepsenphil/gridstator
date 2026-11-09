import { createHash, randomBytes } from 'node:crypto';
import { createReadStream, createWriteStream, existsSync } from 'node:fs';
import { mkdir, readdir, rename, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { Readable, Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { errorMessage, type Logger } from '@gridstator/shared';
import {
  TOOL_NAMES,
  TOOLCHAIN,
  toolArtifactLocation,
  type ClusterSettings,
  type OfferedTool,
  type ToolchainEntry,
  type ToolName,
  type ToolPin,
  type ToolPlatform,
} from '@gridstator/protocol';

/** Opens a download; rejects on HTTP errors. Injected in tests. */
export type Fetcher = (url: string) => Promise<Readable>;

export const httpsFetcher: Fetcher = async (url) => {
  const res = await fetch(url, { redirect: 'follow', signal: AbortSignal.timeout(10 * 60_000) });
  if (!res.ok || !res.body) throw new Error(`GET ${url}: HTTP ${res.status}`);
  return Readable.fromWeb(res.body);
};

/** Retry a failed download after this long. */
const RETRY_MS = 5 * 60_000;

interface Wanted {
  name: ToolName;
  version: string;
  source: 'pinned' | 'override';
  platform: ToolPlatform;
  url: string;
  bin: string;
  /** Known checksum (pinned), or null: read it from the upstream checksum file. */
  sha256: string | null;
  checksumsUrl: string;
  checksumName: string;
}

interface Entry {
  state: 'ready' | 'preparing' | 'failed';
  path: string;
  sha256?: string;
  size?: number;
  reason?: string;
  failedAt?: number;
}

/**
 * Keeps the act and Node.js archives that self-updating workers should run, downloaded once from
 * upstream and verified against the build's pinned sha256 (or, for a version set in the cluster
 * settings, the upstream checksum file). Workers download them from the controller.
 */
export class ToolchainMirror {
  private readonly dir: string;
  private readonly settings: () => ClusterSettings;
  private readonly log: Logger;
  private readonly fetcher: Fetcher;
  private readonly pins: Record<ToolName, ToolPin>;
  private readonly onReady: () => void;
  private readonly entries = new Map<string, Entry>();

  constructor(deps: {
    dir: string;
    settings: () => ClusterSettings;
    logger: Logger;
    fetcher?: Fetcher;
    pins?: Record<ToolName, ToolPin>;
    onReady?: () => void;
  }) {
    this.dir = deps.dir;
    this.settings = deps.settings;
    this.log = deps.logger;
    this.fetcher = deps.fetcher ?? httpsFetcher;
    this.pins = deps.pins ?? TOOLCHAIN;
    this.onReady = deps.onReady ?? (() => undefined);
  }

  private wanted(name: ToolName, platform: ToolPlatform): Wanted | null {
    const override = this.settings().toolchain[name];
    if (override === 'off') return null;
    const loc = toolArtifactLocation(name, override ?? this.pins[name].version, platform);
    if (override === null || override === this.pins[name].version) {
      const a = this.pins[name].artifacts[platform];
      return {
        name,
        version: this.pins[name].version,
        source: override === null ? 'pinned' : 'override',
        platform,
        url: a.url,
        bin: a.bin,
        sha256: a.sha256,
        checksumsUrl: loc.checksumsUrl,
        checksumName: loc.checksumName,
      };
    }
    return { name, version: override, source: 'override', platform, sha256: null, ...loc };
  }

  private key(name: ToolName, version: string, platform: ToolPlatform): string {
    return `${name}-${version}-${platform}`;
  }

  /** The entry for what is wanted now, starting (or retrying) its download if needed. */
  private ensure(w: Wanted): Entry {
    const key = this.key(w.name, w.version, w.platform);
    let e = this.entries.get(key);
    if (e && (e.state !== 'failed' || Date.now() - (e.failedAt ?? 0) < RETRY_MS)) return e;
    e = { state: 'preparing', path: join(this.dir, `${key}.tar.gz`) };
    this.entries.set(key, e);
    const entry = e;
    void this.prepare(w, entry).then(
      () => {
        entry.state = 'ready';
        this.log.info({ tool: key, bytes: entry.size }, 'toolchain archive ready');
        this.onReady();
      },
      (err: unknown) => {
        entry.state = 'failed';
        entry.reason = errorMessage(err);
        entry.failedAt = Date.now();
        this.log.warn({ tool: key, err: entry.reason }, 'could not mirror toolchain archive');
      },
    );
    return entry;
  }

  private async prepare(w: Wanted, e: Entry): Promise<void> {
    await mkdir(this.dir, { recursive: true, mode: 0o700 });
    const expected = w.sha256 ?? (await this.upstreamChecksum(w));
    if (existsSync(e.path) && (await sha256File(e.path)) === expected) {
      e.sha256 = expected;
      e.size = (await stat(e.path)).size;
      return;
    }
    const tmp = `${e.path}.${randomBytes(4).toString('hex')}.tmp`;
    const hash = createHash('sha256');
    let size = 0;
    try {
      await pipeline(
        await this.fetcher(w.url),
        new Transform({
          transform(chunk: Buffer, _enc, cb) {
            hash.update(chunk);
            size += chunk.length;
            cb(null, chunk);
          },
        }),
        createWriteStream(tmp, { mode: 0o600 }),
      );
      const digest = hash.digest('hex');
      if (digest !== expected)
        throw new Error(`${w.url} has sha256 ${digest}, expected ${expected}`);
      await rename(tmp, e.path);
    } finally {
      await rm(tmp, { force: true });
    }
    e.sha256 = expected;
    e.size = size;
    // Older versions of this tool for this platform are no longer offered.
    const prefix = `${w.name}-`;
    const suffix = `-${w.platform}.tar.gz`;
    for (const f of await readdir(this.dir)) {
      if (f.startsWith(prefix) && f.endsWith(suffix) && join(this.dir, f) !== e.path)
        await rm(join(this.dir, f), { force: true });
    }
  }

  private async upstreamChecksum(w: Wanted): Promise<string> {
    const chunks: Buffer[] = [];
    for await (const c of await this.fetcher(w.checksumsUrl)) chunks.push(c as Buffer);
    for (const line of Buffer.concat(chunks).toString('utf8').split('\n')) {
      const m = /^([0-9a-f]{64})\s+\*?(\S+)$/.exec(line.trim());
      if (m && m[2] === w.checksumName) return m[1]!;
    }
    throw new Error(`${w.checksumsUrl} lists no checksum for ${w.checksumName}`);
  }

  /** What the controller offers an agent on `platform`; starts downloads that are missing. */
  offer(platform: ToolPlatform): OfferedTool[] {
    const offered: OfferedTool[] = [];
    for (const name of TOOL_NAMES) {
      const w = this.wanted(name, platform);
      if (!w) continue;
      const e = this.ensure(w);
      offered.push({
        name,
        version: w.version,
        platform,
        state: e.state,
        sha256: e.state === 'ready' ? (e.sha256 ?? null) : null,
        size: e.state === 'ready' ? (e.size ?? null) : null,
        bin: w.bin,
        reason: e.reason ?? null,
      });
    }
    return offered;
  }

  /** Versions an up-to-date worker on `platform` runs: only tools that are ready to hand out. */
  expected(platform: ToolPlatform): Partial<Record<ToolName, string>> {
    const out: Partial<Record<ToolName, string>> = {};
    for (const t of this.offer(platform)) if (t.state === 'ready') out[t.name] = t.version;
    return out;
  }

  /** A ready archive, for the download route. */
  archive(name: ToolName, version: string, platform: ToolPlatform) {
    const e = this.entries.get(this.key(name, version, platform));
    if (!e || e.state !== 'ready' || e.size === undefined) return undefined;
    return { open: () => createReadStream(e.path), size: e.size };
  }

  view(platforms: ToolPlatform[]): ToolchainEntry[] {
    const s = this.settings().toolchain;
    return TOOL_NAMES.map((name) => {
      const override = s[name];
      if (override === 'off') return { name, version: null, source: 'off', platforms: [] };
      const states = platforms.map((platform) => {
        const t = this.offer(platform).find((o) => o.name === name);
        return { platform, state: t?.state ?? 'failed', reason: t?.reason ?? null };
      });
      return {
        name,
        version: override ?? this.pins[name].version,
        source: override === null ? 'pinned' : 'override',
        platforms: states,
      };
    });
  }
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash('sha256');
  await pipeline(createReadStream(path), hash);
  return hash.digest('hex');
}

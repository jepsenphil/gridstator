import { execFile } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { createWriteStream, existsSync, realpathSync } from 'node:fs';
import { chmod, mkdir, readdir, readFile, rename, rm } from 'node:fs/promises';
import { isAbsolute, join, relative, sep } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { promisify } from 'node:util';
import type { GridstatorClient } from '@gridstator/client';
import { readInstallState, setManagedNode, writeFileAtomic, type Logger } from '@gridstator/shared';
import type { AgentTools, OfferedTool, ToolName, ToolStatus } from '@gridstator/protocol';

const execFileAsync = promisify(execFile);

/** How long a Node.js switch has to prove itself before the launcher undoes it. */
const NODE_CONFIRM_MS = 10 * 60_000;

interface Installed {
  version: string;
  /** Executable, relative to the version directory. */
  bin: string;
}

type ToolState = Partial<Record<ToolName, Installed>>;

/**
 * The act and Node.js a self-updating agent runs, installed from archives the controller hands
 * out (`<data dir>/toolchain/<tool>/<version>/`). act is used directly; Node.js is switched by
 * the launcher, which re-executes itself under the managed binary.
 */
export class AgentToolchain {
  readonly dir: string;
  private readonly client: GridstatorClient;
  private readonly log: Logger;
  private readonly installRoot: string | null;
  private state: ToolState = {};

  constructor(opts: {
    dataDir: string;
    client: GridstatorClient;
    logger: Logger;
    /** The managed install's root; null for agents that cannot update themselves. */
    installRoot: string | null;
  }) {
    this.dir = join(opts.dataDir, 'toolchain');
    this.client = opts.client;
    this.log = opts.logger;
    this.installRoot = opts.installRoot;
  }

  private get statePath(): string {
    return join(this.dir, 'state.json');
  }

  async load(): Promise<void> {
    try {
      this.state = JSON.parse(await readFile(this.statePath, 'utf8')) as ToolState;
    } catch {
      this.state = {};
    }
  }

  private async save(): Promise<void> {
    await mkdir(this.dir, { recursive: true, mode: 0o700 });
    await writeFileAtomic(this.statePath, JSON.stringify(this.state, null, 2));
  }

  private binPath(name: ToolName, t: Installed): string {
    return join(this.dir, name, t.version, t.bin);
  }

  /** The managed act binary, if one is installed. */
  actPath(): string | undefined {
    const t = this.state.act;
    if (!t) return undefined;
    const p = this.binPath('act', t);
    return existsSync(p) ? p : undefined;
  }

  private runningManagedNode(): boolean {
    try {
      const exec = realpathSync(process.execPath);
      return !relative(realpathSync(this.dir), exec).startsWith('..');
    } catch {
      return false;
    }
  }

  /** What NodeInfo reports. `actInUse` is the probe of the act binary the agent uses. */
  tools(actInUse: ToolStatus, actIsManaged: boolean): AgentTools {
    const managed: ToolName[] = [];
    if (actIsManaged && actInUse.available) managed.push('act');
    if (this.runningManagedNode()) managed.push('node');
    return {
      act: actInUse.available ? (actInUse.version ?? null) : null,
      node: process.versions.node,
      managed,
    };
  }

  /**
   * Brings act and Node.js to what the controller offers. Returns true when the agent has to
   * restart to use them. Only agents of a managed install manage tools.
   */
  async sync(platform: string, arch: string): Promise<boolean> {
    const root = this.installRoot;
    if (!root) return false;
    const { tools } = await this.client.agentToolchain(platform, arch);
    const offered = new Map(tools.map((t) => [t.name, t]));
    let restart = false;

    const act = offered.get('act');
    if (act?.state === 'ready') {
      const cur = this.state.act;
      if (cur?.version !== act.version || !this.actPath()) {
        await this.install(act);
        this.state.act = { version: act.version, bin: act.bin };
        restart = true;
      }
    } else if (!act && this.state.act) {
      delete this.state.act;
      restart = true;
    }

    const node = offered.get('node');
    const install = await readInstallState(root);
    if (node?.state === 'ready') {
      const want: Installed = { version: node.version, bin: node.bin };
      const path = this.binPath('node', want);
      const running = this.runningManagedNode() && process.versions.node === node.version;
      if (!running) {
        if (install?.node === path && existsSync(path)) {
          throw new Error(
            `the launcher did not switch to Node.js ${node.version} (${path}); see the service log`,
          );
        }
        if (!existsSync(path)) await this.install(node);
        await setManagedNode(root, path, Date.now() + NODE_CONFIRM_MS);
        restart = true;
      }
      this.state.node = want;
    } else if (!node && install?.node) {
      await setManagedNode(root, null, Date.now() + NODE_CONFIRM_MS);
      delete this.state.node;
      restart = true;
    }
    await this.save();
    await this.prune();
    if (restart) this.log.info({ tools: this.state }, 'toolchain updated');
    return restart;
  }

  private async install(t: OfferedTool): Promise<void> {
    if (t.sha256 === null || t.size === null) throw new Error(`${t.name} is not ready`);
    const toolDir = join(this.dir, t.name);
    await mkdir(toolDir, { recursive: true, mode: 0o700 });
    const nonce = randomBytes(6).toString('hex');
    const file = join(toolDir, `.download-${nonce}.tar.gz`);
    const staging = join(toolDir, `.staging-${nonce}`);
    try {
      this.log.info({ tool: t.name, version: t.version }, 'downloading tool');
      const res = await this.client.downloadToolchain(t.name, t.version, t.platform);
      const hash = createHash('sha256');
      let size = 0;
      const expected = t.size;
      await pipeline(
        res,
        new Transform({
          transform(chunk: Buffer, _enc, cb) {
            size += chunk.length;
            if (size > expected) return cb(new Error(`${t.name} archive is larger than announced`));
            hash.update(chunk);
            cb(null, chunk);
          },
        }),
        createWriteStream(file, { mode: 0o600 }),
      );
      const digest = hash.digest('hex');
      if (size !== t.size || digest !== t.sha256)
        throw new Error(`${t.name} ${t.version} archive does not match its checksum`);
      await mkdir(staging, { mode: 0o700 });
      await execFileAsync('tar', ['-xzf', file, '-C', staging, '--no-same-owner']);
      if (isAbsolute(t.bin) || t.bin.split(/[\\/]/).includes('..'))
        throw new Error(`unsafe path ${t.bin} for ${t.name}`);
      const bin = join(staging, t.bin);
      if (!existsSync(bin)) throw new Error(`${t.name} archive has no ${t.bin}`);
      await chmod(bin, 0o755);
      const dest = join(toolDir, t.version);
      await rm(dest, { recursive: true, force: true });
      await rename(staging, dest);
    } finally {
      await rm(file, { force: true });
      await rm(staging, { recursive: true, force: true });
    }
  }

  /** Removes versions that are neither wanted nor running. */
  private async prune(): Promise<void> {
    for (const name of ['act', 'node'] as const) {
      const toolDir = join(this.dir, name);
      if (!existsSync(toolDir)) continue;
      const keep = new Set<string>();
      const wanted = this.state[name];
      if (wanted) keep.add(wanted.version);
      if (name === 'node' && this.runningManagedNode()) {
        const rel = relative(toolDir, realpathSync(process.execPath)).split(sep)[0];
        if (rel) keep.add(rel);
      }
      const install = this.installRoot ? await readInstallState(this.installRoot) : undefined;
      for (const p of [install?.node, install?.pending?.previousNode]) {
        if (p && p.startsWith(toolDir + sep)) keep.add(relative(toolDir, p).split(sep)[0]!);
      }
      for (const v of await readdir(toolDir)) {
        if (!v.startsWith('.') && !keep.has(v))
          await rm(join(toolDir, v), { recursive: true, force: true });
      }
    }
  }
}

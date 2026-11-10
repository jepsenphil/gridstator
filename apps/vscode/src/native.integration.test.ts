import { execFile, spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';
import { ClientConfigSchema, GridstatorClient } from '@gridstator/client';
import { createHash } from 'node:crypto';
import { GridstatorError } from '@gridstator/shared';
import type { Job } from '@gridstator/protocol';
import { NativeCli } from './native';

const binary = process.env.GRIDSTATOR_NATIVE_CLI_E2E;
const execute = promisify(execFile);

it.skipIf(!binary)(
  'actual Rust helper and controller satisfy the editor client contracts',
  async () => {
    if (!binary) throw new Error('GRIDSTATOR_NATIVE_CLI_E2E must select a built native CLI');
    const directory = await mkdtemp(join(tmpdir(), 'gridstator-editor-native-'));
    const project = join(directory, 'workspace with spaces');
    const data = join(directory, 'controller');
    const configPath = join(directory, 'client.json');
    const environment = {
      ...process.env,
      GRIDSTATOR_CONFIG: configPath,
      GRIDSTATOR_CONTROLLER_DATA: data,
      GRIDSTATOR_AGENT_DATA: join(directory, 'agent'),
      GRIDSTATOR_URL: undefined,
      GRIDSTATOR_TOKEN: undefined,
      GRIDSTATOR_CA_FILE: undefined,
    };
    await mkdir(project);
    await writeFile(join(project, 'source.txt'), 'editor native snapshot\n');
    await writeFile(join(project, '.env'), 'PRIVATE_DO_NOT_UPLOAD=value\n');
    await writeFile(
      join(project, 'gridstator.yml'),
      'snapshot: {maxFileSize: 1.0}\ntasks:\n  test:\n    command: echo editor\n',
    );
    await execute(binary, ['controller', 'init', '--host', '127.0.0.1', '--port', '0'], {
      env: environment,
    });
    const controller = spawn(binary, ['controller', 'start'], {
      env: environment,
      stdio: 'ignore',
    });
    const native = new NativeCli({
      enabled: () => true,
      trusted: () => true,
      path: () => binary,
      workspaceRoots: () => [project],
      environment: () => environment,
    });
    let client: GridstatorClient | undefined;
    let worker: ChildProcess | undefined;
    try {
      let port: number | undefined;
      await expect
        .poll(
          async () => {
            try {
              const runtime = JSON.parse(await readFile(join(data, 'runtime.json'), 'utf8')) as {
                port: number;
              };
              port = runtime.port;
              return port > 0;
            } catch {
              return false;
            }
          },
          { timeout: 15_000 },
        )
        .toBe(true);
      const config = ClientConfigSchema.parse(JSON.parse(await readFile(configPath, 'utf8')));
      config.url = `https://127.0.0.1:${port}`;
      await writeFile(configPath, JSON.stringify(config), { mode: 0o600 });
      client = new GridstatorClient({ url: config.url, ca: config.caPem, token: config.token });
      await expect
        .poll(() =>
          client!
            .health()
            .then(() => true)
            .catch(() => false),
        )
        .toBe(true);
      const cluster = await client.cluster();
      expect(cluster.build?.runtime).toBe('rust');
      expect(await client.listNodes()).toEqual([]);
      expect((await client.getSettings()).autoUpdate).toBe('follow');
      expect((await client.updates()).controller.runtime).toBe('rust');
      const discovered = await native.execute<{
        root: string;
        config: { snapshot: { maxFileSize: number } };
      }>({
        operation: 'discover',
        root: project,
      });
      expect(discovered.root).toBe(await realpath(project));
      expect(discovered.config.snapshot.maxFileSize).toBe(1);
      const captured = await native.execute<{ id: string }>({
        operation: 'capture',
        root: project,
        configPath,
        expectedClusterId: cluster.clusterId,
      });
      expect((await client.getSnapshot(captured.id)).fileCount).toBe(2);
      const sourceDigest = `sha256:${createHash('sha256')
        .update('editor native snapshot\n')
        .digest('hex')}`;
      expect((await client.getBlob(sourceDigest)).toString('utf8')).toBe(
        'editor native snapshot\n',
      );
      const job = await native.execute<Job>({
        operation: 'submit',
        root: project,
        configPath,
        expectedClusterId: cluster.clusterId,
        spec: {
          kind: 'command',
          command: ['echo', 'editor'],
          secretEnv: { TOKEN: 'private-native-test' },
        },
      });
      expect((await client.getJob(job.id)).spec.secretEnv.TOKEN).toBe('***');
      expect((await client.jobEvents(job.id)).length).toBeGreaterThan(0);
      expect((await client.cancelJob(job.id)).state).toBe('cancelled');
      const group = await client.submitGroup({
        spec: { kind: 'command', command: ['echo', '{shard}'] },
        shards: 2,
      });
      expect((await client.getGroup(group.id)).jobs).toHaveLength(2);
      expect((await client.cancelGroup(group.id)).state).toBe('cancelled');
      const joinToken = await client.createToken({
        name: 'editor-native-join',
        kind: 'join',
        maxUses: 1,
      });
      await execute(
        binary,
        [
          'agent',
          'join',
          config.url,
          '--token',
          joinToken.token,
          '--fingerprint',
          config.caFingerprint,
          '--name',
          'editor-native-worker',
        ],
        { env: environment },
      );
      worker = spawn(binary, ['agent', 'start'], { env: environment, stdio: 'ignore' });
      await expect
        .poll(
          async () => {
            const nodes = await client!.listNodes();
            return nodes.find(
              (node) => node.name === 'editor-native-worker' && node.status === 'online',
            )?.info?.build?.runtime;
          },
          { timeout: 20_000 },
        )
        .toBe('rust');
      const local = await native.execute({
        operation: 'local',
        configPath,
        args: ['status', '--data-dir', data],
      });
      expect(JSON.stringify(local)).not.toContain(config.token);
    } catch (error) {
      if (error instanceof GridstatorError && error.details) {
        throw new Error(`${error.message}: ${JSON.stringify(error.details)}`, { cause: error });
      }
      throw error;
    } finally {
      native.dispose();
      client?.close();
      for (const child of [worker, controller]) {
        if (child && child.exitCode === null && child.signalCode === null) {
          const exited = once(child, 'exit');
          const force = setTimeout(() => child.kill('SIGKILL'), 15_000);
          child.kill('SIGTERM');
          await exited;
          clearTimeout(force);
        }
      }
      await rm(directory, { recursive: true, force: true });
    }
  },
  60_000,
);

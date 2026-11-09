// Runs the extension inside a real VS Code instance against a real controller + Docker worker.
// Usage: xvfb-run -a node test/run-integration.mjs   (from apps/vscode, after building)
import { execFileSync, spawn } from 'node:child_process';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runTests } from '@vscode/test-electron';

// When launched from a terminal inside VS Code this is set and would make Electron behave as Node.
delete process.env.ELECTRON_RUN_AS_NODE;

const here = dirname(fileURLToPath(import.meta.url));
const extensionPath = join(here, '..');
const repo = join(extensionPath, '..', '..');
const cli = join(repo, 'apps/cli/dist/main.js');

const port = await new Promise((resolve) => {
  const s = createServer();
  s.listen(0, '127.0.0.1', () => {
    const p = s.address().port;
    s.close(() => resolve(p));
  });
});
const root = await mkdtemp(join(tmpdir(), 'gst-vscode-it-'));
const env = {
  ...process.env,
  GRIDSTATOR_CONTROLLER_DATA: join(root, 'controller'),
  GRIDSTATOR_CONFIG: join(root, 'client.json'),
  GRIDSTATOR_CACHE: join(root, 'cache'),
  GRIDSTATOR_LOG_FORMAT: 'json',
};
// GRIDSTATOR_IT_CONFIG points at an existing cluster's client.json (e.g. a real multi-machine
// cluster) and GRIDSTATOR_IT_NODE names the worker to run on; otherwise a local cluster is started.
const external = process.env.GRIDSTATOR_IT_CONFIG;
const targetNode = process.env.GRIDSTATOR_IT_NODE ?? 'it-worker';
if (external) env.GRIDSTATOR_CONFIG = external;
let controller;
let controllerLog = '';
if (!external) {
  execFileSync(
    process.execPath,
    [cli, 'controller', 'init', '--host', '127.0.0.1', '--port', String(port)],
    { env, stdio: 'ignore' },
  );
  controller = spawn(
    process.execPath,
    [cli, 'controller', 'start', '--with-agent', '--agent-name', 'it-worker'],
    { env, stdio: ['ignore', 'ignore', 'pipe'] },
  );
  controller.stderr.on('data', (d) => (controllerLog += d.toString()));
}

let code = 1;
try {
  for (let i = 0; i < 60; i++) {
    try {
      execFileSync(process.execPath, [cli, 'nodes', '--json'], { env, stdio: 'pipe' });
      if (
        JSON.parse(
          execFileSync(process.execPath, [cli, 'nodes', '--json'], { env }).toString(),
        ).some((n) => n.status === 'online')
      )
        break;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 500));
  }

  // Workspace: the example project plus a deliberately failing test and a JUnit-reporting task.
  const workspace = join(root, 'workspace');
  await cp(join(repo, 'examples/node-project'), workspace, { recursive: true });
  await writeFile(
    join(workspace, 'test', 'broken.test.js'),
    "import assert from 'node:assert/strict';\nimport { test } from 'node:test';\nimport { add } from '../src/math.js';\n\ntest('broken addition', () => {\n  assert.equal(add(2, 2), 5);\n});\n",
  );
  const config = await readFile(join(workspace, 'gridstator.yml'), 'utf8');
  await writeFile(
    join(workspace, 'gridstator.yml'),
    config.replace(
      'tasks:\n',
      'tasks:\n  junit:\n    description: Tests with a JUnit report\n    command: mkdir -p reports && node --test --test-reporter=spec --test-reporter-destination=stdout --test-reporter=junit --test-reporter-destination=reports/junit.xml\n    junit: reports/junit.xml\n',
    ),
  );
  execFileSync('git', ['init', '-q'], { cwd: workspace });
  execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'add', '-A'], {
    cwd: workspace,
  });
  execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init'], {
    cwd: workspace,
  });

  code = await runTests({
    extensionDevelopmentPath: extensionPath,
    extensionTestsPath: join(here, 'suite', 'index.cjs'),
    launchArgs: [
      workspace,
      '--disable-extensions',
      '--disable-gpu',
      '--user-data-dir',
      join(root, 'user-data'),
      '--skip-welcome',
      '--skip-release-notes',
    ],
    extensionTestsEnv: {
      GRIDSTATOR_CONTROLLER_DATA: env.GRIDSTATOR_CONTROLLER_DATA,
      GRIDSTATOR_CONFIG: env.GRIDSTATOR_CONFIG,
      GRIDSTATOR_CACHE: env.GRIDSTATOR_CACHE,
      GRIDSTATOR_IT_NODE: targetNode,
    },
  }).catch((err) => {
    process.stderr.write(`VS Code tests failed: ${err}\n`);
    return 1;
  });
} finally {
  if (controller) {
    controller.kill('SIGTERM');
    await new Promise((r) => controller.once('exit', r));
  }
  if (code !== 0) process.stderr.write(controllerLog.slice(-4000));
  await rm(root, { recursive: true, force: true });
}
process.exit(code);

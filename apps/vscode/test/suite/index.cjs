// Runs inside the VS Code extension host. Drives the Gridstator extension through its commands and
// public API against a real controller and worker, and asserts on VS Code state (Problems, Testing).
const assert = require('node:assert/strict');
const vscode = require('vscode');

async function waitFor(fn, what, timeoutMs = 120_000) {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

function log(msg) {
  process.stdout.write(`[gridstator-it] ${msg}\n`);
}

exports.run = async function run() {
  const ext = vscode.extensions.getExtension('gridstator.gridstator-vscode');
  assert.ok(ext, 'extension is installed');
  const api = await ext.activate();
  await vscode.workspace
    .getConfiguration('gridstator')
    .update('unsavedFiles', 'ignore', vscode.ConfigurationTarget.Workspace);
  await vscode.workspace
    .getConfiguration('gridstator')
    .update('notifications', 'none', vscode.ConfigurationTarget.Workspace);
  const target = process.env.GRIDSTATOR_IT_NODE || 'it-worker';
  // Jobs from this window go to the chosen worker (the "Select Target Worker" setting).
  await vscode.workspace
    .getConfiguration('gridstator')
    .update('defaultNode', target, vscode.ConfigurationTarget.Workspace);

  await waitFor(() => api.state.connection.status === 'connected', 'connection');
  log(`connected as ${api.state.connection.user}`);
  const node = await waitFor(
    () => api.state.nodeList().find((n) => n.status === 'online' && n.name === target),
    `node ${target} online`,
  );
  log(
    `target worker ${node.name}: ${node.info.cpus} CPU, ${Math.round(node.info.memoryTotalMb / 1024)} GiB`,
  );

  await api.launcher.refresh();
  const ids = api.launcher.items.map((i) => i.id);
  log(`discovered ${ids.length} launch items`);
  const folder = vscode.workspace.workspaceFolders[0].name;
  for (const expected of [
    `task:${folder}:junit`,
    `task:${folder}:test`,
    `script:${folder}:.:test`,
  ]) {
    assert.ok(ids.includes(expected), `launcher has ${expected} (got ${ids.join(', ')})`);
  }

  // The dashboard opens (webview content is covered by its own build and type checks).
  await vscode.commands.executeCommand('gridstator.openDashboard');

  // Run the JUnit task: one test fails on purpose.
  const job = await api.launcher.launch({ itemId: `task:${folder}:junit`, options: {} });
  log(`submitted ${job.id}`);
  const done = await api.results.waitFor(job.id);
  assert.equal(done.state, 'failed', `job failed as expected (error: ${done.error})`);
  assert.equal(done.nodeName, target);
  log(`ran on ${done.nodeName}`);
  const finished = await waitFor(async () => {
    const j = await api.state.refreshJob(job.id);
    return j && j.artifacts.length ? j : undefined;
  }, 'artifacts');
  assert.deepEqual(
    finished.artifacts.map((a) => a.name),
    ['reports/junit.xml.tar'],
  );

  // Results flow into the Testing view and the Problems panel.
  const outcome = await api.results.process(finished);
  log(`results: ${outcome.tests} tests, ${outcome.problems} problems`);
  assert.ok(outcome.tests >= 3, 'JUnit cases were parsed');
  const suites = [];
  api.results.tests.items.forEach((s) => suites.push(s));
  const names = suites.flatMap((s) => {
    const out = [];
    s.children.forEach((c) => out.push(c.label));
    return out;
  });
  assert.ok(
    names.includes('broken addition'),
    `test items include the failing test (${names.join(', ')})`,
  );
  const diags = vscode.languages
    .getDiagnostics()
    .filter(([uri, list]) => uri.fsPath.endsWith('broken.test.js') && list.length > 0);
  assert.equal(diags.length, 1, 'the failing test is in the Problems panel');
  assert.equal(diags[0][1][0].range.start.line, 5, 'problem points at the failing assertion');

  // Cancel a running job via the command used by the Jobs view.
  const long = await api.launcher.launch({
    itemId: `command:${folder}`,
    options: { command: 'echo started; sleep 300' },
  });
  await waitFor(() => api.state.jobs.get(long.id)?.state === 'running', 'long job running');
  await vscode.commands.executeCommand('gridstator.job.cancel', api.state.jobs.get(long.id));
  await waitFor(() => api.state.jobs.get(long.id)?.state === 'cancelled', 'cancellation', 60_000);
  log('cancelled');

  // Retry creates a new attempt linked to the original.
  const before = api.state.myJobs.size;
  await vscode.commands.executeCommand('gridstator.job.retry', api.state.jobs.get(long.id));
  await waitFor(() => api.state.myJobs.size === before + 1, 'retry');
  const retried = api.state.jobList().find((j) => j.retryOf === long.id);
  assert.ok(retried, 'retry job exists');
  await api.state.requireClient().cancelJob(retried.id);
  log(`retried as ${retried.id}`);

  // Node management commands.
  const byName = () => api.state.nodeList().find((n) => n.name === target);
  await vscode.commands.executeCommand('gridstator.node.drain', byName());
  await waitFor(() => byName().settings.schedulingMode === 'draining', 'drain');
  await vscode.commands.executeCommand('gridstator.node.enable', byName());
  await waitFor(() => byName().settings.schedulingMode === 'enabled', 'enable');
  log('node management ok');
};

import { Readable } from 'node:stream';
import tar from 'tar-stream';
import { unzipSync } from 'fflate';
import * as vscode from 'vscode';
import type { Artifact, Job } from '@gridstator/protocol';
import type { GridstatorClient } from '@gridstator/client';
import { looksLikeJUnit, parseJUnit, type JUnitCase } from './junit';
import type { JobOrigin, Launcher } from './launcher';
import { mapRemotePath, matchProblems } from './problems';
import type { ClusterState } from './state';

const MAX_REPORT_BYTES = 20 * 1024 * 1024;
const MAX_LOG_LINES = 200_000;
const STACK =
  /\(?((?:\/workspace\/|\/[^\s()]*\/work\/[0-9A-Z]{26}\/src\/)[^\s():]+):(\d+):(\d+)\)?/;

async function readAll(stream: AsyncIterable<unknown>): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const c of stream) chunks.push(Buffer.from(c as Uint8Array));
  return Buffer.concat(chunks);
}

/** Pulls XML documents out of an artifact (raw .xml, a .tar from command jobs, or a .zip from actions). */
export async function xmlDocuments(name: string, data: Buffer): Promise<string[]> {
  if (name.endsWith('.xml')) return [data.toString('utf8')];
  if (name.endsWith('.zip')) {
    const files = unzipSync(new Uint8Array(data));
    return Object.entries(files)
      .filter(([n]) => n.endsWith('.xml'))
      .map(([, v]) => Buffer.from(v).toString('utf8'));
  }
  if (name.endsWith('.tar')) {
    const extract = tar.extract();
    const out: string[] = [];
    const done = (async () => {
      for await (const entry of extract) {
        const body = await readAll(entry);
        if (entry.header.type === 'file' && entry.header.name.endsWith('.xml'))
          out.push(body.toString('utf8'));
      }
    })();
    Readable.from([data]).pipe(extract);
    await done;
    return out;
  }
  return [];
}

function candidateReports(artifacts: readonly Artifact[]): Artifact[] {
  return artifacts.filter(
    (a) => a.size <= MAX_REPORT_BYTES && /\.(xml|zip|tar)$/.test(a.name) && a.name !== 'image.tar',
  );
}

/**
 * Turns finished jobs into IDE feedback: problems from logs (Problems panel) and JUnit results
 * from artifacts (Testing view). Only jobs started from this window are processed.
 */
export class ResultsIntegration implements vscode.Disposable {
  readonly diagnostics = vscode.languages.createDiagnosticCollection('gridstator');
  readonly tests: vscode.TestController;
  private readonly disposables: vscode.Disposable[] = [];
  private readonly waiters = new Map<string, (job: Job) => void>();

  constructor(
    private readonly state: ClusterState,
    private readonly launcher: Launcher,
    private readonly log: vscode.OutputChannel,
  ) {
    this.tests = vscode.tests.createTestController('gridstator', 'Gridstator (remote runs)');
    this.tests.createRunProfile(
      'Run on Gridstator',
      vscode.TestRunProfileKind.Run,
      (request, token) => this.runTests(request, token),
      true,
    );
    this.disposables.push(
      this.diagnostics,
      this.tests,
      state.onJobFinished((job) => {
        this.waiters.get(job.id)?.(job);
        this.waiters.delete(job.id);
        if (!state.myJobs.has(job.id)) return;
        void this.process(job).catch((err: unknown) =>
          this.log.appendLine(`Result processing for ${job.id} failed: ${(err as Error).message}`),
        );
      }),
    );
  }

  waitFor(jobId: string): Promise<Job> {
    return new Promise((resolve) => this.waiters.set(jobId, resolve));
  }

  clear(): void {
    this.diagnostics.clear();
  }

  private origin(job: Job): JobOrigin | undefined {
    return this.launcher.origins.get(job.id);
  }

  async process(job: Job): Promise<{ problems: number; tests: number }> {
    const client = this.state.client;
    const origin = this.origin(job);
    if (!client || !origin) return { problems: 0, tests: 0 };
    const cfg = vscode.workspace.getConfiguration('gridstator');
    let problems = 0;
    let tests = 0;
    if (cfg.get<boolean>('problemMatching', true))
      problems = await this.problemsFromLogs(client, job, origin);
    if (cfg.get<boolean>('testResults', true))
      tests = await this.testsFromArtifacts(client, job, origin);
    return { problems, tests };
  }

  private async problemsFromLogs(
    client: GridstatorClient,
    job: Job,
    origin: JobOrigin,
  ): Promise<number> {
    const lines: string[] = [];
    let after = 0;
    for (;;) {
      const page = await client.jobLogs(job.id, after, 10_000);
      for (const e of page.entries) if (e.o !== 'system') lines.push(...e.d.split('\n'));
      if (page.entries.length < 10_000 || lines.length > MAX_LOG_LINES) break;
      after = page.entries[page.entries.length - 1]!.s;
    }
    const found = matchProblems(lines, {
      root: origin.root,
      workdir: origin.workdir,
      jobId: job.id,
    });
    // Replace earlier remote problems for this project with this run's.
    this.diagnostics.forEach((uri) => {
      if (uri.fsPath.startsWith(origin.root)) this.diagnostics.delete(uri);
    });
    const byFile = new Map<string, vscode.Diagnostic[]>();
    for (const p of found) {
      const range = new vscode.Range(
        Math.max(0, p.line - 1),
        Math.max(0, p.column - 1),
        Math.max(0, p.line - 1),
        Math.max(0, p.column - 1) + 1,
      );
      const d = new vscode.Diagnostic(
        range,
        p.message,
        p.severity === 'warning'
          ? vscode.DiagnosticSeverity.Warning
          : p.severity === 'info'
            ? vscode.DiagnosticSeverity.Information
            : vscode.DiagnosticSeverity.Error,
      );
      d.source = `gridstator ${p.source}`;
      if (p.code) d.code = p.code;
      const list = byFile.get(p.file) ?? [];
      list.push(d);
      byFile.set(p.file, list);
    }
    for (const [file, list] of byFile) this.diagnostics.set(vscode.Uri.file(file), list);
    if (found.length > 0)
      this.log.appendLine(`Job ${job.id}: ${found.length} problem(s) added to the Problems panel`);
    return found.length;
  }

  private async testsFromArtifacts(
    client: GridstatorClient,
    job: Job,
    origin: JobOrigin,
  ): Promise<number> {
    const cases: JUnitCase[] = [];
    for (const a of candidateReports(job.artifacts)) {
      try {
        const data = await readAll(await client.downloadArtifact(job.id, a.name));
        for (const xml of await xmlDocuments(a.name, data))
          if (looksLikeJUnit(xml)) cases.push(...parseJUnit(xml));
      } catch (err) {
        this.log.appendLine(
          `Could not read artifact ${a.name} of ${job.id}: ${(err as Error).message}`,
        );
      }
    }
    if (cases.length === 0) return 0;
    this.publishTestRun(job, origin, cases);
    return cases.length;
  }

  private location(tc: JUnitCase, origin: JobOrigin, jobId: string): vscode.Location | undefined {
    const ctx = { root: origin.root, workdir: origin.workdir, jobId };
    const stack = tc.details ? STACK.exec(tc.details) : null;
    if (stack) {
      const file = mapRemotePath(stack[1]!, ctx);
      if (file)
        return new vscode.Location(
          vscode.Uri.file(file),
          new vscode.Position(Math.max(0, +stack[2]! - 1), Math.max(0, +stack[3]! - 1)),
        );
    }
    for (const candidate of [tc.file, tc.classname, tc.suite.split(' › ')[0]]) {
      if (!candidate) continue;
      const file = mapRemotePath(candidate, ctx);
      if (file)
        return new vscode.Location(
          vscode.Uri.file(file),
          new vscode.Position(Math.max(0, (tc.line ?? 1) - 1), 0),
        );
    }
    return undefined;
  }

  private publishTestRun(
    job: Job,
    origin: JobOrigin,
    cases: JUnitCase[],
    run?: vscode.TestRun,
  ): void {
    const ownRun =
      run ??
      this.tests.createTestRun(
        new vscode.TestRunRequest(),
        `Gridstator ${job.name} (${job.nodeName ?? 'remote'})`,
        false,
      );
    for (const tc of cases) {
      const suiteId = `${origin.folder}::${tc.suite}`;
      let suite = this.tests.items.get(suiteId);
      if (!suite) {
        suite = this.tests.createTestItem(suiteId, tc.suite);
        this.tests.items.add(suite);
      }
      const id = `${suiteId}::${tc.name}`;
      let item = suite.children.get(id);
      const loc = this.location(tc, origin, job.id);
      if (!item) {
        item = this.tests.createTestItem(id, tc.name, loc?.uri);
        suite.children.add(item);
      }
      if (loc) item.range = loc.range;
      ownRun.enqueued(item);
      if (tc.status === 'passed') ownRun.passed(item, tc.timeMs);
      else if (tc.status === 'skipped') ownRun.skipped(item);
      else {
        const msg = new vscode.TestMessage(
          `${tc.message ?? 'failed'}${tc.details ? `\n\n${tc.details}` : ''}`,
        );
        if (loc) msg.location = loc;
        ownRun.failed(item, msg, tc.timeMs);
      }
    }
    ownRun.appendOutput(
      `Results from Gridstator job ${job.id} on ${job.nodeName ?? 'a remote node'}\r\n`,
    );
    if (!run) ownRun.end();
  }

  /** Testing view "Run on Gridstator": runs the project's JUnit task remotely and reports results. */
  private async runTests(
    request: vscode.TestRunRequest,
    token: vscode.CancellationToken,
  ): Promise<void> {
    const item = this.launcher.items.find((i) => i.kind === 'task' && i.task?.junit);
    if (!item) {
      void vscode.window.showInformationMessage(
        'Add a task with a `junit:` report path to gridstator.yml (for example `test: { command: npm test -- --reporter=junit --outputFile=junit.xml, junit: junit.xml }`) to run tests on Gridstator from the Testing view.',
      );
      return;
    }
    const run = this.tests.createTestRun(request, `Gridstator: ${item.label}`, false);
    try {
      const job = await this.launcher.launch({ itemId: item.id, options: {} });
      run.appendOutput(`Submitted job ${job.id}\r\n`);
      token.onCancellationRequested(
        () => void this.state.client?.cancelJob(job.id).catch(() => undefined),
      );
      const done = await this.waitFor(job.id);
      const origin = this.launcher.origins.get(done.id);
      const client = this.state.client;
      if (origin && client) {
        const cases: JUnitCase[] = [];
        for (const a of candidateReports(done.artifacts)) {
          const data = await readAll(await client.downloadArtifact(done.id, a.name));
          for (const xml of await xmlDocuments(a.name, data))
            if (looksLikeJUnit(xml)) cases.push(...parseJUnit(xml));
        }
        this.publishTestRun(done, origin, cases, run);
      }
      run.appendOutput(`Job ${done.id} ${done.state}${done.error ? `: ${done.error}` : ''}\r\n`);
    } catch (err) {
      run.appendOutput(`${(err as Error).message}\r\n`);
    } finally {
      run.end();
    }
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}

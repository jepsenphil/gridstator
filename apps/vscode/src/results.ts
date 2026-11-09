import * as vscode from 'vscode';
import type { Job, JobGroup } from '@gridstator/protocol';
import type { GridstatorClient } from '@gridstator/client';
import { collectJUnit, summarize, type JUnitCase, type TestSummary } from '@gridstator/reports';
import { isGroup, type JobOrigin, type Launcher } from './launcher';
import { mapRemotePath, matchProblems, type Problem } from './problems';
import type { ClusterState } from './state';

const MAX_LOG_LINES = 200_000;
const STACK =
  /\(?((?:\/workspace\/|\/[^\s()]*\/work\/[0-9A-Z]{26}\/src\/)[^\s():]+):(\d+):(\d+)\)?/;

interface CaseFromJob {
  job: Job;
  cases: JUnitCase[];
}

/**
 * Turns finished jobs into IDE feedback: problems from logs (Problems panel) and JUnit results
 * from artifacts (Testing view). Only jobs started from this window are processed; shards of a job
 * group are processed together when the whole group finishes.
 */
export class ResultsIntegration implements vscode.Disposable {
  readonly diagnostics = vscode.languages.createDiagnosticCollection('gridstator');
  readonly tests: vscode.TestController;
  private readonly disposables: vscode.Disposable[] = [];
  private readonly waiters = new Map<string, (job: Job) => void>();
  private readonly groupWaiters = new Map<string, (group: JobGroup) => void>();

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
    const report = (what: string) => (err: unknown) =>
      this.log.appendLine(`Result processing for ${what} failed: ${(err as Error).message}`);
    this.disposables.push(
      this.diagnostics,
      this.tests,
      state.onJobFinished((job) => {
        this.waiters.get(job.id)?.(job);
        this.waiters.delete(job.id);
        if (!state.myJobs.has(job.id) || job.groupId) return;
        void this.process(job).catch(report(job.id));
      }),
      state.onGroupFinished((group) => {
        this.groupWaiters.get(group.id)?.(group);
        this.groupWaiters.delete(group.id);
        if (!state.myGroups.has(group.id)) return;
        void this.processGroup(group).catch(report(group.id));
      }),
    );
  }

  waitFor(jobId: string): Promise<Job> {
    return new Promise((resolve) => this.waiters.set(jobId, resolve));
  }

  waitForGroup(groupId: string): Promise<JobGroup> {
    return new Promise((resolve) => this.groupWaiters.set(groupId, resolve));
  }

  clear(): void {
    this.diagnostics.clear();
  }

  private enabled(key: 'problemMatching' | 'testResults'): boolean {
    return vscode.workspace.getConfiguration('gridstator').get<boolean>(key, true);
  }

  /** Problems and test results for one finished job. */
  async process(job: Job): Promise<{ problems: number; tests: number }> {
    const client = this.state.client;
    const origin = this.launcher.origins.get(job.id);
    if (!client || !origin) return { problems: 0, tests: 0 };
    const problems = this.enabled('problemMatching')
      ? this.publishProblems(origin, await this.problemsFor(client, job, origin))
      : 0;
    let tests = 0;
    if (this.enabled('testResults')) {
      const cases = await this.casesFor(client, job);
      if (cases.length > 0)
        this.publishTestRun(`Gridstator ${job.name} (${job.nodeName ?? 'remote'})`, origin, [
          { job, cases },
        ]);
      tests = cases.length;
    }
    return { problems, tests };
  }

  /** Problems and combined test results for every shard of a finished group. */
  async processGroup(group: JobGroup): Promise<{ problems: number; tests: number }> {
    const client = this.state.client;
    const origin =
      this.launcher.groupOrigins.get(group.id) ??
      this.launcher.origins.get(group.jobs[0]?.id ?? '');
    if (!client || !origin) return { problems: 0, tests: 0 };
    let problems = 0;
    if (this.enabled('problemMatching')) {
      const all: Problem[] = [];
      for (const job of group.jobs) all.push(...(await this.problemsFor(client, job, origin)));
      problems = this.publishProblems(origin, all);
    }
    let tests = 0;
    if (this.enabled('testResults')) {
      const sets: CaseFromJob[] = [];
      for (const job of group.jobs) sets.push({ job, cases: await this.casesFor(client, job) });
      tests = sets.reduce((n, s) => n + s.cases.length, 0);
      if (tests > 0)
        this.publishTestRun(
          `Gridstator ${group.name} (${group.shardTotal} shards on ${group.nodes.join(', ')})`,
          origin,
          sets,
        );
    }
    return { problems, tests };
  }

  /** Combined JUnit summary across a group's shards (for the dashboard). */
  async groupSummary(group: JobGroup): Promise<TestSummary | null> {
    const client = this.state.client;
    if (!client) return null;
    const sets = [];
    for (const job of group.jobs) {
      const cases = await this.casesFor(client, job);
      if (cases.length)
        sets.push({ ...(job.shardIndex !== null ? { shard: job.shardIndex } : {}), cases });
    }
    return sets.length ? summarize(sets) : null;
  }

  private async problemsFor(
    client: GridstatorClient,
    job: Job,
    origin: JobOrigin,
  ): Promise<Problem[]> {
    const lines: string[] = [];
    let after = 0;
    for (;;) {
      const page = await client.jobLogs(job.id, after, 10_000);
      for (const e of page.entries) if (e.o !== 'system') lines.push(...e.d.split('\n'));
      if (page.entries.length < 10_000 || lines.length > MAX_LOG_LINES) break;
      after = page.entries[page.entries.length - 1]!.s;
    }
    return matchProblems(lines, { root: origin.root, workdir: origin.workdir, jobId: job.id });
  }

  /** Replaces this project's remote problems with `found`. */
  private publishProblems(origin: JobOrigin, found: Problem[]): number {
    this.diagnostics.forEach((uri) => {
      if (uri.fsPath.startsWith(origin.root)) this.diagnostics.delete(uri);
    });
    const byFile = new Map<string, vscode.Diagnostic[]>();
    for (const p of found) {
      const line = Math.max(0, p.line - 1);
      const col = Math.max(0, p.column - 1);
      const d = new vscode.Diagnostic(
        new vscode.Range(line, col, line, col + 1),
        p.message,
        p.severity === 'warning'
          ? vscode.DiagnosticSeverity.Warning
          : p.severity === 'info'
            ? vscode.DiagnosticSeverity.Information
            : vscode.DiagnosticSeverity.Error,
      );
      d.source = `gridstator ${p.source}`;
      if (p.code) d.code = p.code;
      byFile.set(p.file, [...(byFile.get(p.file) ?? []), d]);
    }
    for (const [file, list] of byFile) this.diagnostics.set(vscode.Uri.file(file), list);
    if (found.length > 0)
      this.log.appendLine(`${found.length} problem(s) added to the Problems panel`);
    return found.length;
  }

  private async casesFor(client: GridstatorClient, job: Job): Promise<JUnitCase[]> {
    try {
      return await collectJUnit(job.artifacts, (a) => client.downloadArtifact(job.id, a.name));
    } catch (err) {
      this.log.appendLine(`Could not read test reports of ${job.id}: ${(err as Error).message}`);
      return [];
    }
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
    title: string,
    origin: JobOrigin,
    sets: CaseFromJob[],
    run?: vscode.TestRun,
  ): void {
    const ownRun = run ?? this.tests.createTestRun(new vscode.TestRunRequest(), title, false);
    for (const { job, cases } of sets) {
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
          const shard = job.shardIndex
            ? ` [shard ${job.shardIndex}/${job.shardTotal} on ${job.nodeName}]`
            : '';
          const msg = new vscode.TestMessage(
            `${tc.message ?? 'failed'}${shard}${tc.details ? `\n\n${tc.details}` : ''}`,
          );
          if (loc) msg.location = loc;
          ownRun.failed(item, msg, tc.timeMs);
        }
      }
      ownRun.appendOutput(
        `Results from Gridstator job ${job.id} on ${job.nodeName ?? 'a remote node'}\r\n`,
      );
    }
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
      const launched = await this.launcher.launch({ itemId: item.id, options: {} });
      const origin = isGroup(launched)
        ? this.launcher.groupOrigins.get(launched.id)
        : this.launcher.origins.get(launched.id);
      const client = this.state.client;
      if (isGroup(launched)) {
        run.appendOutput(`Submitted group ${launched.id} (${launched.shardTotal} shards)\r\n`);
        token.onCancellationRequested(
          () => void client?.cancelGroup(launched.id).catch(() => undefined),
        );
        const done = await this.waitForGroup(launched.id);
        if (origin && client) {
          const sets: CaseFromJob[] = [];
          for (const job of done.jobs) sets.push({ job, cases: await this.casesFor(client, job) });
          this.publishTestRun('', origin, sets, run);
        }
        run.appendOutput(`Group ${done.id} ${done.state}\r\n`);
      } else {
        run.appendOutput(`Submitted job ${launched.id}\r\n`);
        token.onCancellationRequested(
          () => void client?.cancelJob(launched.id).catch(() => undefined),
        );
        const done = await this.waitFor(launched.id);
        if (origin && client)
          this.publishTestRun(
            '',
            origin,
            [{ job: done, cases: await this.casesFor(client, done) }],
            run,
          );
        run.appendOutput(`Job ${done.id} ${done.state}${done.error ? `: ${done.error}` : ''}\r\n`);
      }
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

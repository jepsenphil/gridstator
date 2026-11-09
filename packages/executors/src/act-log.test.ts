import { describe, expect, it } from 'vitest';
import { ActLogParser, LineSplitter } from './act-log.js';

const line = (o: Record<string, unknown>) =>
  JSON.stringify({
    dryrun: false,
    level: 'info',
    matrix: {},
    time: '2026-11-09T13:21:30+13:00',
    ...o,
  });

describe('ActLogParser', () => {
  it('renders act JSON logs in act console format and tracks steps', () => {
    const p = new ActLogParser();
    const lines = [
      line({ msg: "Using docker host 'unix:///var/run/docker.sock'" }),
      line({
        job: 'CI/test',
        jobID: 'test',
        msg: '⭐ Run Set up job',
        step: 'Set up job',
        stepid: ['--setup-job'],
      }),
      line({
        job: 'CI/test',
        jobID: 'test',
        msg: '  ✅  Success - Set up job',
        step: 'Set up job',
        stepResult: 'success',
        stepid: ['--setup-job'],
      }),
      line({
        job: 'CI/test',
        jobID: 'test',
        msg: '⭐ Run Main Test',
        stage: 'Main',
        step: 'Test',
        stepID: ['1'],
      }),
      line({
        job: 'CI/test',
        jobID: 'test',
        msg: 'line one\nline two\n',
        raw_output: true,
        stage: 'Main',
        step: 'Test',
        stepID: ['1'],
      }),
      line({
        job: 'CI/test',
        jobID: 'test',
        msg: '  ❌  Failure - Main Test',
        stage: 'Main',
        step: 'Test',
        stepID: ['1'],
        stepResult: 'failure',
        executionTime: 2_500_000_000,
      }),
      line({ job: 'CI/test', jobID: 'test', msg: '🏁  Job failed', jobResult: 'failure' }),
      line({ job: 'CI/test', jobID: 'test', level: 'debug', msg: 'noise' }),
    ];
    const rendered = lines.flatMap((l) => p.feed(l, 'stdout'));
    expect(rendered).toEqual([
      { stream: 'system', text: "act: Using docker host 'unix:///var/run/docker.sock'\n" },
      { stream: 'stdout', text: '[CI/test] ⭐ Run Set up job\n' },
      { stream: 'stdout', text: '[CI/test] ✅  Success - Set up job\n' },
      { stream: 'stdout', text: '[CI/test] ⭐ Run Main Test\n' },
      { stream: 'stdout', text: '[CI/test]   | line one\n' },
      { stream: 'stdout', text: '[CI/test]   | line two\n' },
      { stream: 'stdout', text: '[CI/test] ❌  Failure - Main Test\n' },
      { stream: 'stdout', text: '[CI/test] 🏁  Job failed\n' },
    ]);
    expect(p.getSteps().map((s) => [s.group, s.name, s.status, s.durationMs])).toEqual([
      ['CI/test', 'Set up job', 'success', 0],
      ['CI/test', 'Test', 'failure', 2500],
    ]);
    expect(p.failedJobs()).toEqual(['CI/test']);
    expect(p.takeChanged()).toBe(true);
    expect(p.takeChanged()).toBe(false);
  });

  it('passes through non-JSON output and labels Pre/Post stages', () => {
    const p = new ActLogParser();
    expect(p.feed('plain text', 'stderr')).toEqual([{ stream: 'stderr', text: 'plain text\n' }]);
    p.feed(
      line({ job: 'W/j', msg: '⭐ Run Post Cache', stage: 'Post', step: 'Cache', stepID: ['2'] }),
      'stdout',
    );
    expect(p.getSteps()[0]?.name).toBe('Post Cache');
    p.finishRunning('skipped');
    expect(p.getSteps()[0]?.status).toBe('skipped');
  });

  it('marks interrupted steps as cancelled', () => {
    const p = new ActLogParser();
    p.feed(
      line({ job: 'W/j', msg: '⭐ Run Main Long', stage: 'Main', step: 'Long', stepID: ['0'] }),
      'stdout',
    );
    p.feed(
      line({
        job: 'W/j',
        msg: 'x',
        stage: 'Main',
        step: 'Long',
        stepID: ['0'],
        stepResult: 'failure',
        executionTime: 5e9,
      }),
      'stdout',
    );
    p.markCancelled(Date.now());
    expect(p.getSteps()[0]?.status).toBe('cancelled');
  });
});

describe('LineSplitter', () => {
  it('reassembles lines and multi-byte characters split across chunks', () => {
    const s = new LineSplitter();
    const lines: string[] = [];
    const bytes = Buffer.from('héllo\nwörld\npartial');
    s.push(bytes.subarray(0, 2), (l) => lines.push(l));
    s.push(bytes.subarray(2, 9), (l) => lines.push(l));
    s.push(bytes.subarray(9), (l) => lines.push(l));
    s.end((l) => lines.push(l));
    expect(lines).toEqual(['héllo', 'wörld', 'partial']);
  });
});

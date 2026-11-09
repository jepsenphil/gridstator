import { existsSync } from 'node:fs';
import { isAbsolute, join, posix } from 'node:path';

export type Severity = 'error' | 'warning' | 'info';

export interface Problem {
  /** Absolute local path. */
  file: string;
  /** 1-based. */
  line: number;
  /** 1-based. */
  column: number;
  severity: Severity;
  message: string;
  source: string;
  code?: string;
}

/**
 * Maps a path printed by a remote job to a local file: container paths (`/workspace/...`), act's
 * staged snapshot (`.../work/<jobId>/src/...`), or paths relative to the job's working directory.
 * Returns undefined unless the file exists locally.
 */
export function mapRemotePath(
  p: string,
  ctx: { root: string; workdir: string; jobId: string },
): string | undefined {
  const clean = p.replace(/^file:\/\//, '').trim();
  let rel: string | undefined;
  if (clean.startsWith('/workspace/')) rel = clean.slice('/workspace/'.length);
  else {
    const m = new RegExp(`/work/${ctx.jobId}/src/(.+)$`).exec(clean);
    if (m) rel = m[1];
    else if (!isAbsolute(clean) && !/^[A-Za-z]:/.test(clean))
      rel = posix.join(ctx.workdir === '.' ? '' : ctx.workdir, clean);
  }
  if (rel === undefined) return undefined;
  const normalized = posix.normalize(rel);
  if (normalized.startsWith('..') || normalized.includes('node_modules/')) return undefined;
  const local = join(ctx.root, normalized);
  return existsSync(local) ? local : undefined;
}

// eslint-disable-next-line no-control-regex
const ANSI = /\u001b\[[0-9;?]*[A-Za-z]/g;
const TSC =
  /^(?<file>[^\s(][^(]*?)\((?<line>\d+),(?<col>\d+)\): (?<sev>error|warning) (?<code>TS\d+): (?<msg>.+)$/;
const COLON =
  /^(?<file>[^\s:]+\.[A-Za-z0-9]+):(?<line>\d+):(?<col>\d+)(?::| -)\s*(?:(?<sev>error|warning|note|info)\b[:\s]*)?(?<msg>.+)$/i;
const ESLINT_ROW =
  /^\s+(?<line>\d+):(?<col>\d+)\s+(?<sev>error|warning)\s+(?<msg>.+?)(?:\s{2,}(?<rule>[@\w/-]+))?\s*$/;
const STACK =
  /\(?(?<file>(?:\/workspace\/|\/[^\s()]*\/work\/[0-9A-Z]{26}\/src\/)[^\s():]+):(?<line>\d+):(?<col>\d+)\)?/;
const FAILURE_HINT = /(AssertionError|Error:|✖|×|FAIL|failed|expected)/;

/**
 * Extracts problems from job output. Supports `tsc`, `file:line:col` style tools (eslint unix,
 * gcc/clang, ruff, mypy, go vet), ESLint's default "stylish" output, and the first workspace frame
 * of failing test stack traces.
 */
export function matchProblems(
  lines: readonly string[],
  ctx: { root: string; workdir: string; jobId: string },
): Problem[] {
  const out: Problem[] = [];
  const seen = new Set<string>();
  let eslintFile: string | undefined;
  let lastFailureMessage: string | undefined;
  const push = (p: Problem) => {
    const key = `${p.file}:${p.line}:${p.column}:${p.message}`;
    if (seen.has(key)) return;
    seen.add(key);
    out.push(p);
  };
  for (const raw of lines) {
    // act prefixes step output with "[Workflow/job]   | ".
    const line = raw
      .replace(ANSI, '')
      .replace(/^\[[^\]]+\]\s+\|\s?/, '')
      .replace(/\r$/, '');
    let m = TSC.exec(line);
    if (m?.groups) {
      const file = mapRemotePath(m.groups['file']!, ctx);
      if (file) {
        push({
          file,
          line: +m.groups['line']!,
          column: +m.groups['col']!,
          severity: m.groups['sev'] === 'warning' ? 'warning' : 'error',
          message: m.groups['msg']!,
          source: 'tsc',
          code: m.groups['code']!,
        });
      }
      continue;
    }
    // ESLint stylish: a bare file path followed by indented "line:col severity message rule" rows.
    if (/^\S/.test(line) && !line.includes(' ')) {
      const file = mapRemotePath(line, ctx);
      eslintFile = file;
      if (file) continue;
    }
    if (eslintFile) {
      m = ESLINT_ROW.exec(line);
      if (m?.groups) {
        push({
          file: eslintFile,
          line: +m.groups['line']!,
          column: +m.groups['col']!,
          severity: m.groups['sev'] === 'warning' ? 'warning' : 'error',
          message: m.groups['msg']!,
          source: 'eslint',
          ...(m.groups['rule'] ? { code: m.groups['rule'] } : {}),
        });
        continue;
      }
      if (line.trim() === '') continue;
      eslintFile = undefined;
    }
    m = COLON.exec(line.trim());
    if (m?.groups && m.groups['sev']) {
      const file = mapRemotePath(m.groups['file']!, ctx);
      if (file) {
        const sev = m.groups['sev'].toLowerCase();
        push({
          file,
          line: +m.groups['line']!,
          column: +m.groups['col']!,
          severity: sev === 'warning' ? 'warning' : sev === 'error' ? 'error' : 'info',
          message: m.groups['msg']!.trim(),
          source: 'remote',
        });
        continue;
      }
    }
    if (FAILURE_HINT.test(line) && !STACK.test(line)) lastFailureMessage = line.trim();
    m = STACK.exec(line);
    if (m?.groups && lastFailureMessage) {
      const file = mapRemotePath(m.groups['file']!, ctx);
      if (file) {
        push({
          file,
          line: +m.groups['line']!,
          column: +m.groups['col']!,
          severity: 'error',
          message: lastFailureMessage,
          source: 'test',
        });
        lastFailureMessage = undefined;
      }
    }
  }
  return out;
}

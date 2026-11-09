import { XMLParser } from 'fast-xml-parser';

export interface JUnitCase {
  suite: string;
  name: string;
  classname?: string;
  file?: string;
  line?: number;
  timeMs?: number;
  status: 'passed' | 'failed' | 'skipped';
  message?: string;
  details?: string;
}

type Node = Record<string, unknown>;
const asArray = <T>(v: T | T[] | undefined): T[] =>
  v === undefined ? [] : Array.isArray(v) ? v : [v];
const str = (v: unknown): string | undefined =>
  typeof v === 'string' ? v : typeof v === 'number' ? String(v) : undefined;

function text(v: unknown): string | undefined {
  if (typeof v === 'string') return v;
  if (v && typeof v === 'object') return str((v as Node)['#text']);
  return undefined;
}

/** Parses JUnit XML (Jest, Vitest, Node's test runner, Playwright, pytest, Maven, …). */
export function parseJUnit(xml: string): JUnitCase[] {
  const parser = new XMLParser({
    ignoreAttributes: false,
    attributeNamePrefix: '',
    textNodeName: '#text',
    allowBooleanAttributes: true,
  });
  const doc = parser.parse(xml) as Node;
  const cases: JUnitCase[] = [];
  const visitSuite = (suite: Node, parents: string[]) => {
    const name = str(suite['name']) ?? 'tests';
    if (parents.length === 0 && suite['__root'] === true) {
      // Test cases directly under <testsuites> (Node's reporter does this): group them by file.
      for (const tc of asArray(suite['testcase'] as Node | Node[] | undefined)) {
        const file = str(tc['file']);
        visitSuite({ name: file ? (file.split('/').pop() ?? file) : 'tests', testcase: tc }, []);
      }
      for (const inner of asArray(suite['testsuite'] as Node | Node[] | undefined))
        visitSuite(inner, []);
      return;
    }
    const path = [...parents, name];
    for (const inner of asArray(suite['testsuite'] as Node | Node[] | undefined))
      visitSuite(inner, path);
    for (const tc of asArray(suite['testcase'] as Node | Node[] | undefined)) {
      const failure = asArray(tc['failure'])[0] ?? asArray(tc['error'])[0];
      const skipped = tc['skipped'] !== undefined;
      const f = failure && typeof failure === 'object' ? (failure as Node) : undefined;
      const lineAttr = str(tc['line']);
      const timeAttr = str(tc['time']);
      cases.push({
        suite: path.join(' › '),
        name: str(tc['name']) ?? 'test',
        ...(str(tc['classname']) ? { classname: str(tc['classname'])! } : {}),
        ...(str(tc['file']) ? { file: str(tc['file'])! } : {}),
        ...(lineAttr && /^\d+$/.test(lineAttr) ? { line: Number(lineAttr) } : {}),
        ...(timeAttr && !Number.isNaN(Number(timeAttr))
          ? { timeMs: Math.round(Number(timeAttr) * 1000) }
          : {}),
        status: failure !== undefined ? 'failed' : skipped ? 'skipped' : 'passed',
        ...(failure !== undefined
          ? {
              message:
                str(f?.['message']) ??
                (typeof failure === 'string' ? failure.split('\n')[0] : 'failed'),
            }
          : {}),
        ...(failure !== undefined ? { details: text(failure) ?? '' } : {}),
      });
    }
  };
  const root = doc['testsuites'] as Node | undefined;
  if (root && typeof root === 'object') visitSuite({ ...root, __root: true }, []);
  for (const s of asArray(doc['testsuite'] as Node | Node[] | undefined)) visitSuite(s, []);
  return cases;
}

export function looksLikeJUnit(text: string): boolean {
  return /<testsuites?[\s>]/.test(text.slice(0, 4096));
}

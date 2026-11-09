import { Readable } from 'node:stream';
import { unzipSync } from 'fflate';
import tar from 'tar-stream';
import { looksLikeJUnit, parseJUnit, type JUnitCase } from './junit.js';

/** Reports larger than this are not downloaded for parsing. */
export const MAX_REPORT_BYTES = 20 * 1024 * 1024;

export interface ArtifactRef {
  name: string;
  size: number;
}

export async function readAll(stream: AsyncIterable<unknown>): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const c of stream) chunks.push(Buffer.from(c as Uint8Array));
  return Buffer.concat(chunks);
}

/** Pulls XML documents out of an artifact: raw .xml, a .tar (command jobs) or a .zip (workflow uploads). */
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

/** Artifacts that may contain JUnit reports and are small enough to fetch. */
export function candidateReports<T extends ArtifactRef>(artifacts: readonly T[]): T[] {
  return artifacts.filter(
    (a) => a.size <= MAX_REPORT_BYTES && /\.(xml|zip|tar)$/.test(a.name) && a.name !== 'image.tar',
  );
}

/** Downloads candidate artifacts with `download` and parses every JUnit document found. */
export async function collectJUnit<T extends ArtifactRef>(
  artifacts: readonly T[],
  download: (artifact: T) => Promise<AsyncIterable<unknown>>,
): Promise<JUnitCase[]> {
  const cases: JUnitCase[] = [];
  for (const a of candidateReports(artifacts)) {
    const data = await readAll(await download(a));
    for (const xml of await xmlDocuments(a.name, data))
      if (looksLikeJUnit(xml)) cases.push(...parseJUnit(xml));
  }
  return cases;
}

export interface TestSummary {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
  /** Total test time in ms (sum of test case durations). */
  timeMs: number;
  failures: Array<{ suite: string; name: string; message?: string; shard?: number }>;
}

/** Summarises test cases, optionally tagged with the shard they came from. */
export function summarize(
  sets: ReadonlyArray<{ shard?: number; cases: readonly JUnitCase[] }>,
): TestSummary {
  const s: TestSummary = { total: 0, passed: 0, failed: 0, skipped: 0, timeMs: 0, failures: [] };
  for (const set of sets) {
    for (const c of set.cases) {
      s.total++;
      s.timeMs += c.timeMs ?? 0;
      if (c.status === 'passed') s.passed++;
      else if (c.status === 'skipped') s.skipped++;
      else {
        s.failed++;
        s.failures.push({
          suite: c.suite,
          name: c.name,
          ...(c.message ? { message: c.message } : {}),
          ...(set.shard !== undefined ? { shard: set.shard } : {}),
        });
      }
    }
  }
  return s;
}

import { Readable } from 'node:stream';
import tar from 'tar-stream';
import { zipSync } from 'fflate';
import { describe, expect, it, vi } from 'vitest';
import { looksLikeJUnit, parseJUnit } from './junit';

vi.mock('vscode', () => ({}));
const { xmlDocuments } = await import('./results');

const NODE_JUNIT = `<?xml version="1.0" encoding="utf-8"?>
<testsuites>
  <testsuite name="math" tests="3" failures="1" skipped="1">
    <testcase name="adds" classname="test" time="0.0012" file="/workspace/test/math.test.js" line="4"/>
    <testcase name="subtracts" classname="test" time="0.002">
      <failure message="Expected 1 to equal 2" type="AssertionError">AssertionError: Expected 1 to equal 2
    at TestContext.&lt;anonymous&gt; (/workspace/test/math.test.js:9:10)</failure>
    </testcase>
    <testcase name="later" classname="test"><skipped/></testcase>
    <testsuite name="nested"><testcase name="deep" time="0.5"/></testsuite>
  </testsuite>
</testsuites>`;

describe('parseJUnit', () => {
  it('parses suites, nested suites, failures, skips, files and durations', () => {
    expect(looksLikeJUnit(NODE_JUNIT)).toBe(true);
    expect(looksLikeJUnit('<html/>')).toBe(false);
    const cases = parseJUnit(NODE_JUNIT);
    expect(cases.map((c) => [c.suite, c.name, c.status, c.timeMs])).toEqual([
      ['math › nested', 'deep', 'passed', 500],
      ['math', 'adds', 'passed', 1],
      ['math', 'subtracts', 'failed', 2],
      ['math', 'later', 'skipped', undefined],
    ]);
    expect(cases[1]).toMatchObject({ file: '/workspace/test/math.test.js', line: 4 });
    expect(cases[2]?.message).toBe('Expected 1 to equal 2');
    expect(cases[2]?.details).toContain('/workspace/test/math.test.js:9:10');
  });

  it('handles test cases directly under <testsuites> (Node.js reporter)', () => {
    const xml = `<?xml version="1.0" encoding="utf-8"?>
<testsuites>
	<testcase name="add" time="0.000880" classname="test" file="/workspace/test/math.test.js"/>
	<testcase name="broken" time="0.001" classname="test" file="/workspace/test/broken.test.js">
		<failure type="testCodeFailure" message="Expected values to be strictly equal">AssertionError</failure>
	</testcase>
	<!-- tests 2 -->
</testsuites>`;
    expect(parseJUnit(xml).map((c) => [c.suite, c.name, c.status])).toEqual([
      ['math.test.js', 'add', 'passed'],
      ['broken.test.js', 'broken', 'failed'],
    ]);
  });

  it('handles a single top-level testsuite', () => {
    expect(parseJUnit('<testsuite name="s"><testcase name="t"/></testsuite>')).toEqual([
      { suite: 's', name: 't', status: 'passed' },
    ]);
  });
});

describe('xmlDocuments', () => {
  it('extracts XML from raw, tar (command artifacts) and zip (actions artifacts)', async () => {
    expect(await xmlDocuments('junit.xml', Buffer.from('<testsuite/>'))).toEqual(['<testsuite/>']);
    const zip = zipSync({
      'report/junit.xml': Buffer.from('<testsuites/>'),
      'summary.txt': Buffer.from('x'),
    });
    expect(await xmlDocuments('report.zip', Buffer.from(zip))).toEqual(['<testsuites/>']);
    const pack = tar.pack();
    pack.entry({ name: 'reports/junit.xml' }, '<testsuites name="t"/>');
    pack.entry({ name: 'reports/other.txt' }, 'nope');
    pack.finalize();
    const chunks: Buffer[] = [];
    for await (const c of Readable.from(pack as AsyncIterable<Buffer>)) chunks.push(c as Buffer);
    expect(await xmlDocuments('reports/junit.xml.tar', Buffer.concat(chunks))).toEqual([
      '<testsuites name="t"/>',
    ]);
  });
});

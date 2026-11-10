#!/usr/bin/env node
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const reference = resolve(process.argv[2] ?? '');
const output = process.argv[3];
if (!output) throw new Error('Usage: capture-client-urls.mjs FROZEN_CHECKOUT OUTPUT');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
  cwd: reference,
  encoding: 'utf8',
}).trim();
if (revision !== '4b8f13ccf1b21820007fef0ab52143c961b69216') throw new Error('Wrong reference');
const { normalizeControllerUrl } = await import(
  pathToFileURL(join(reference, 'packages/client/dist/http.js'))
);
const inputs = [
  'controller',
  'controller.local',
  'controller:7312',
  '127.0.0.1',
  '127.0.0.1:7312',
  '[::1]',
  '[::1]:7312',
  'https://[2001:db8::1]:443',
  'https://[2001:db8::1]:7443',
  'https://controller',
  'https://controller:443',
  'https://controller:7300',
  'https://controller:0',
  'https://controller:65535',
  'https://controller:65536',
  ' HTTPS://CONTROLLER:7312/path?query=value#fragment ',
  '\uFEFFcontroller\uFEFF',
  '\t\ncontroller\r\n',
  '\u0085controller\u0085',
  'https://user:password@controller/path?q=secret#secret',
  'https://controller/a/b///',
  'http://controller:7300/path',
  'HTTP://controller',
  'https://',
  '',
  'a b',
  'controller:invalid',
  '::1',
  'https://[invalid]',
  'https://münich.example',
  'https://127.1',
  'https://2130706433',
  'https://0x7f000001',
  'https://controller:0443',
  'https://controller:07300',
  'ftp://controller',
  'https://user%40name:password@controller',
  'https://controller\\path',
];
const cases = inputs.map((input, index) => {
  try {
    return {
      id: `url-${String(index + 1).padStart(3, '0')}`,
      input,
      value: normalizeControllerUrl(input),
    };
  } catch (error) {
    return {
      id: `url-${String(index + 1).padStart(3, '0')}`,
      input,
      error: {
        code: error.code,
        message: error.message,
        ...(error.hint ? { hint: error.hint } : {}),
      },
    };
  }
});
const path = 'packages/client/src/http.ts';
writeFileSync(
  output,
  `${JSON.stringify(
    {
      formatVersion: 1,
      sourceRevision: revision,
      source: {
        path,
        sha256: createHash('sha256')
          .update(readFileSync(join(reference, path)))
          .digest('hex'),
      },
      cases,
    },
    null,
    2,
  )}\n`,
);

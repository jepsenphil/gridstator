import { execFileSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { formatConnectionCommand, workerControllerUrlError } from './connection-command';

const PIN = `sha256:${'ab'.repeat(32)}`;

function shellArguments(command: string): string[] {
  return execFileSync('/bin/sh', ['-c', command.replace(/^gridstator /, 'printf "%s\\n" ')], {
    encoding: 'utf8',
  })
    .trimEnd()
    .split('\n');
}

describe('connection command formatting', () => {
  it('produces one desktop-compatible join command and preserves the controller port', () => {
    const command = formatConnectionCommand(
      'join',
      'https://192.168.1.77:7443',
      PIN,
      'gsj_worker_credential',
    );
    expect(command).toBe(
      `gridstator agent join 'https://192.168.1.77:7443' --fingerprint '${PIN}' --token 'gsj_worker_credential'`,
    );
  });

  it('keeps a client login distinct from worker enrollment', () => {
    expect(formatConnectionCommand('client', 'https://controller:7300', PIN, 'gsc_test')).toBe(
      `gridstator login 'https://controller:7300' --fingerprint '${PIN}' --token 'gsc_test'`,
    );
  });

  it.skipIf(process.platform === 'win32')(
    'passes shell metacharacters and quotes as literal argument data',
    () => {
      const token = "gsj_'$(printf substituted); echo expanded";
      expect(
        shellArguments(formatConnectionCommand('join', 'https://controller:7300', PIN, token)),
      ).toEqual([
        'agent',
        'join',
        'https://controller:7300',
        '--fingerprint',
        PIN,
        '--token',
        token,
      ]);
    },
  );
});

describe('worker controller URL validation', () => {
  it.each([
    'https://192.168.1.77:7300',
    'https://10.1.2.3:7443/',
    'https://controller.local:7300',
    'https://127.controller.example:7300',
    'https://build-server.example.com',
    'https://[fd12:3456::abcd]:7300',
    'https://[2001:db8::1]:7443/',
  ])('accepts a LAN IP or hostname (%s)', (value) => {
    expect(workerControllerUrlError(value)).toBeNull();
  });

  it.each([
    'https://localhost:7300',
    'https://LOCALHOST.:7300',
    'https://controller.localhost:7300',
    'https://127.0.0.1:7300',
    'https://127.42.13.7:7300',
    'https://127.1:7300',
    'https://2130706433:7300',
    'https://0x7f000001:7300',
    'https://0177.0.0.1:7300',
    'https://0.0.0.0:7300',
    'https://0:7300',
    'https://[::]:7300',
    'https://[::1]:7300',
    'https://[0:0:0:0:0:0:0:1]:7300',
    'https://[::ffff:127.0.0.1]:7300',
    'https://[::ffff:0.0.0.0]:7300',
    'https://*:7300',
    'https://*.example.com:7300',
  ])('requires an address reachable from another machine (%s)', (value) => {
    expect(workerControllerUrlError(value)).toContain('LAN IP address or hostname');
  });

  it.each([
    '',
    'controller:7300',
    'http://controller:7300',
    'https:/controller:7300',
    'https://user:password@controller:7300',
    'https://controller:7300/path',
    'https://controller:7300/path/..',
    'https://controller:7300?token=private',
    'https://controller:7300?',
    'https://controller:7300#fragment',
    'https://controller:7300#',
    ' https://controller:7300',
    'https://controller:7300 ',
    'https://con\ntroller:7300',
    'https://controller:7300\\path',
    'https://controller:99999',
    'https://[not-an-address]:7300',
    'https://controller:7300\0',
  ])('rejects malformed endpoints without echoing their contents (%s)', (value) => {
    expect(workerControllerUrlError(value)).toBe(
      'Enter an HTTPS controller URL without credentials, a path, query parameters or spaces.',
    );
  });

  it('rejects port zero', () => {
    expect(workerControllerUrlError('https://controller:0')).toContain('1 to 65535');
  });
});

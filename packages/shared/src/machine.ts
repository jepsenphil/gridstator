import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { hostname, platform } from 'node:os';

let identity: Promise<string> | undefined;

/** Stable, opaque host identity shared by the controller and worker on the same machine. */
export function machineIdentity(): Promise<string> {
  identity ??= readMachineIdentity();
  return identity;
}

async function readMachineIdentity(): Promise<string> {
  for (const path of ['/etc/machine-id', '/var/lib/dbus/machine-id']) {
    try {
      const value = (await readFile(path, 'utf8')).trim();
      if (value) return hash(value);
    } catch {
      // Other platforms and restricted installations can still identify this host by its name.
    }
  }
  return hash(`${platform()}:${hostname()}`);
}

function hash(value: string): string {
  return createHash('sha256').update(`gridstator-machine:${value}`).digest('hex');
}

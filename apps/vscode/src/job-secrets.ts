import { createHash } from 'node:crypto';
import { basename, resolve } from 'node:path';
import { homedir } from 'node:os';
import * as vscode from 'vscode';
import { GridstatorError, machineIdentity } from '@gridstator/shared';

type SecretStorage = Pick<vscode.SecretStorage, 'get' | 'store' | 'delete'>;

export class SecretInputCancelled extends Error {
  constructor() {
    super('Secret entry cancelled');
    this.name = 'SecretInputCancelled';
  }
}

function requireTrustedWorkspace(): void {
  if (!vscode.workspace.isTrusted) {
    throw new GridstatorError('FORBIDDEN', 'Trust this workspace before accessing job secrets');
  }
}

/** Include the extension host and account because SecretStorage is shared by remote windows. */
function storageKey(hostIdentity: string, root: string, name: string): string {
  const scope = createHash('sha256')
    .update(JSON.stringify([hostIdentity, homedir(), resolve(root), name]))
    .digest('hex');
  return `gridstator.job-secret.v1.${scope}`;
}

/** Secret values stay in the extension host and VS Code's encrypted secret storage. */
export class JobSecrets {
  constructor(
    private readonly storage: SecretStorage,
    private readonly identity: () => Promise<string> = machineIdentity,
  ) {}

  async resolve(root: string, names: readonly string[]): Promise<Record<string, string>> {
    requireTrustedWorkspace();
    const entries: Array<[string, string]> = [];
    for (const name of new Set(names)) {
      const environmentValue = process.env[name];
      if (environmentValue) {
        entries.push([name, environmentValue]);
        continue;
      }
      let key: string;
      let value: string | undefined;
      try {
        key = storageKey(await this.identity(), root, name);
        value = await this.storage.get(key);
      } catch {
        throw new GridstatorError('CONFIG', 'Unable to read encrypted job secret storage', {
          hint: 'Unlock the OS keychain or credential store and try again.',
        });
      }
      if (!value) {
        value = await vscode.window.showInputBox({
          title: 'Gridstator: Save job secret',
          prompt: `Enter ${name} for ${basename(resolve(root))}. It will be saved securely for future runs.`,
          password: true,
          ignoreFocusOut: true,
          validateInput: (input) => (input.trim() ? undefined : 'Enter a non-empty secret'),
        });
        if (value === undefined) throw new SecretInputCancelled();
        // Native controls validate input, but keep the boundary safe for alternate hosts as well.
        if (!value.trim()) throw new GridstatorError('BAD_REQUEST', 'Enter a non-empty job secret');
        try {
          await this.storage.store(key, value);
        } catch {
          throw new GridstatorError('CONFIG', 'Unable to save the encrypted job secret', {
            hint: 'Unlock the OS keychain or credential store and try again.',
          });
        }
      }
      entries.push([name, value]);
    }
    return Object.fromEntries(entries);
  }

  async clear(root: string, names: readonly string[]): Promise<void> {
    requireTrustedWorkspace();
    for (const name of new Set(names)) {
      try {
        await this.storage.delete(storageKey(await this.identity(), root, name));
      } catch {
        throw new GridstatorError('CONFIG', 'Unable to remove the encrypted job secret', {
          hint: 'Unlock the OS keychain or credential store and try again.',
        });
      }
    }
  }
}

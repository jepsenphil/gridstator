import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const host = vi.hoisted(() => ({
  workspace: { isTrusted: true },
  window: { showInputBox: vi.fn() },
}));
vi.mock('vscode', () => host);

import { JobSecrets, SecretInputCancelled } from './job-secrets';

const NAME = 'GRIDSTATOR_JOB_SECRETS_TEST';
const OTHER = 'GRIDSTATOR_JOB_SECRETS_OTHER_TEST';
const ROOT = '/workspace/first-project';
const VALUE = 'private-job-secret';

function storage() {
  const values = new Map<string, string>();
  return {
    values,
    get: vi.fn(async (key: string) => values.get(key)),
    store: vi.fn(async (key: string, value: string) => {
      values.set(key, value);
    }),
    delete: vi.fn(async (key: string) => {
      values.delete(key);
    }),
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  host.workspace.isTrusted = true;
  vi.stubEnv(NAME, undefined);
  vi.stubEnv(OTHER, undefined);
});
afterEach(() => vi.unstubAllEnvs());

describe('project job secrets', () => {
  it('prompts securely once and reuses encrypted storage across resolver instances', async () => {
    const saved = storage();
    host.window.showInputBox.mockResolvedValue(VALUE);
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME])).resolves.toEqual({ [NAME]: VALUE });
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME])).resolves.toEqual({ [NAME]: VALUE });
    expect(host.window.showInputBox).toHaveBeenCalledTimes(1);
    expect(host.window.showInputBox).toHaveBeenCalledWith(
      expect.objectContaining({ password: true, ignoreFocusOut: true }),
    );
    expect(saved.store).toHaveBeenCalledTimes(1);
    expect(process.env[NAME]).toBeUndefined();
  });

  it('does not grant another project access to a saved secret with the same name', async () => {
    const saved = storage();
    host.window.showInputBox.mockResolvedValueOnce(VALUE).mockResolvedValueOnce('second-value');
    const secrets = new JobSecrets(saved);
    await secrets.resolve(ROOT, [NAME]);
    await expect(secrets.resolve('/workspace/second-project', [NAME])).resolves.toEqual({
      [NAME]: 'second-value',
    });
    expect(saved.values.size).toBe(2);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(2);
  });

  it('isolates identical project paths on different extension hosts when resolving and clearing', async () => {
    const saved = storage();
    const firstHost = new JobSecrets(saved, async () => 'first-host');
    const secondHost = new JobSecrets(saved, async () => 'second-host');
    host.window.showInputBox
      .mockResolvedValueOnce('first-value')
      .mockResolvedValueOnce('second-value');
    await expect(firstHost.resolve(ROOT, [NAME])).resolves.toEqual({ [NAME]: 'first-value' });
    await expect(secondHost.resolve(ROOT, [NAME])).resolves.toEqual({ [NAME]: 'second-value' });
    expect(saved.values.size).toBe(2);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(2);
    await firstHost.clear(ROOT, [NAME]);
    await expect(
      new JobSecrets(saved, async () => 'second-host').resolve(ROOT, [NAME]),
    ).resolves.toEqual({
      [NAME]: 'second-value',
    });
    expect(host.window.showInputBox).toHaveBeenCalledTimes(2);
    host.window.showInputBox.mockResolvedValueOnce('replacement-value');
    await expect(firstHost.resolve(ROOT, [NAME])).resolves.toEqual({ [NAME]: 'replacement-value' });
    expect(host.window.showInputBox).toHaveBeenCalledTimes(3);
  });

  it('retains environment precedence without reading or overwriting storage', async () => {
    const saved = storage();
    vi.stubEnv(NAME, 'environment-value');
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME])).resolves.toEqual({
      [NAME]: 'environment-value',
    });
    expect(saved.get).not.toHaveBeenCalled();
    expect(saved.store).not.toHaveBeenCalled();
    expect(host.window.showInputBox).not.toHaveBeenCalled();
  });

  it('treats an empty environment variable as missing and deduplicates configured names', async () => {
    const saved = storage();
    vi.stubEnv(NAME, '');
    host.window.showInputBox.mockResolvedValue(VALUE);
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME, NAME])).resolves.toEqual({
      [NAME]: VALUE,
    });
    expect(saved.get).toHaveBeenCalledTimes(1);
    expect(saved.store).toHaveBeenCalledTimes(1);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(1);
  });

  it('cancels without saving a value or collecting later secrets', async () => {
    const saved = storage();
    host.window.showInputBox.mockResolvedValue(undefined);
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME, OTHER])).rejects.toBeInstanceOf(
      SecretInputCancelled,
    );
    expect(saved.store).not.toHaveBeenCalled();
    expect(saved.get).toHaveBeenCalledTimes(1);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(1);
  });

  it('validates empty input without exposing the entered value', async () => {
    const saved = storage();
    host.window.showInputBox.mockResolvedValue('  ');
    await expect(new JobSecrets(saved).resolve(ROOT, [NAME])).rejects.toThrow(
      'Enter a non-empty job secret',
    );
    const options = host.window.showInputBox.mock.calls[0]?.[0] as {
      validateInput: (value: string) => string | undefined;
    };
    expect(options.validateInput('')).toBe('Enter a non-empty secret');
    expect(options.validateInput(VALUE)).toBeUndefined();
    expect(saved.store).not.toHaveBeenCalled();
  });

  it.each(['get', 'store', 'delete'] as const)(
    'sanitizes %s storage errors instead of echoing underlying diagnostics',
    async (operation) => {
      const saved = storage();
      saved[operation].mockRejectedValue(new Error(`storage diagnostic contains ${VALUE}`));
      host.window.showInputBox.mockResolvedValue(VALUE);
      const secrets = new JobSecrets(saved);
      const promise =
        operation === 'delete' ? secrets.clear(ROOT, [NAME]) : secrets.resolve(ROOT, [NAME]);
      await expect(promise).rejects.toMatchObject({
        code: 'CONFIG',
        message: expect.not.stringContaining(VALUE),
      });
      await expect(promise).rejects.not.toHaveProperty('cause');
    },
  );

  it('refuses all access in an untrusted workspace before reading storage or prompting', async () => {
    const saved = storage();
    host.workspace.isTrusted = false;
    vi.stubEnv(NAME, 'environment-value');
    const secrets = new JobSecrets(saved);
    await expect(secrets.resolve(ROOT, [NAME])).rejects.toMatchObject({ code: 'FORBIDDEN' });
    await expect(secrets.clear(ROOT, [NAME])).rejects.toMatchObject({ code: 'FORBIDDEN' });
    expect(saved.get).not.toHaveBeenCalled();
    expect(saved.delete).not.toHaveBeenCalled();
    expect(host.window.showInputBox).not.toHaveBeenCalled();
  });

  it('clears only the selected project and prompts again on its next run', async () => {
    const saved = storage();
    const secrets = new JobSecrets(saved);
    host.window.showInputBox.mockResolvedValue(VALUE);
    await secrets.resolve(ROOT, [NAME]);
    await secrets.resolve('/workspace/second-project', [NAME]);
    await secrets.clear(ROOT, [NAME, NAME]);
    expect(saved.delete).toHaveBeenCalledTimes(1);
    await secrets.resolve('/workspace/second-project', [NAME]);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(2);
    await secrets.resolve(ROOT, [NAME]);
    expect(host.window.showInputBox).toHaveBeenCalledTimes(3);
  });
});

import { beforeEach, describe, expect, it, vi } from 'vitest';

const prompts = vi.hoisted(() => ({ showQuickPick: vi.fn(), showInputBox: vi.fn() }));
vi.mock('vscode', () => ({ window: prompts }));

import { parseConnection, promptConnection } from './sign-in';

const PIN = `sha256:${'a1'.repeat(32)}`;
const TOKEN = 'gsc_private_credential';
const URL = 'https://controller:7300';
const connection = { url: URL, fingerprint: PIN, token: TOKEN };

beforeEach(() => {
  vi.resetAllMocks();
});

describe('connection paste parser', () => {
  it.each([
    `gridstator login ${URL} --fingerprint ${PIN} --token ${TOKEN}`,
    `gridstator login '${URL}/' --token="${TOKEN}" --fingerprint='${PIN}'`,
    `gridstator login --token ${TOKEN} ${URL} --fingerprint=${PIN}`,
    `'/path with spaces/gridstator' login ${URL} --fingerprint ${PIN} --token ${TOKEN}`,
    `gridstator login ${URL} \\\n      --fingerprint ${PIN} \\\n      --token ${TOKEN}`,
    `${URL} ${PIN} ${TOKEN}`,
    `${URL} ${PIN} ${TOKEN}\r\n`,
    `"${URL}" '${'A1:'.repeat(31)}A1' "${TOKEN}"`,
  ])('accepts command and string forms (%#)', (input) => {
    expect(parseConnection(input)).toEqual(connection);
  });

  it('normalizes an IPv6 URL and its default port', () => {
    expect(parseConnection(`https://[::1] ${PIN} ${TOKEN}`).url).toBe('https://[::1]:7300');
  });

  it.each([
    '',
    `gridstator login ${URL} --fingerprint ${PIN}`,
    `gridstator login ${URL} --fingerprint ${PIN} --token`,
    `gridstator login ${URL} --fingerprint ${PIN} --token ${TOKEN} --token duplicate`,
    `gridstator login ${URL} --fingerprint ${PIN} --token ${TOKEN} --config /tmp/other`,
    `gridstator login ${URL} --fingerprint ${PIN} --token ${TOKEN}; touch /tmp/sentinel`,
    `gridstator login ${URL} --fingerprint ${PIN} --token ${TOKEN} && echo private`,
    `gridstator login ${URL} --fingerprint ${PIN} --token $(echo ${TOKEN})`,
    `gridstator login ${URL} --fingerprint ${PIN} --token '${TOKEN}`,
    `curl ${URL} --fingerprint ${PIN} --token ${TOKEN}`,
    `${URL}/path ${PIN} ${TOKEN}`,
    `${URL}?token=${TOKEN} ${PIN} ${TOKEN}`,
    `https://user:${TOKEN}@controller:7300 ${PIN} ${TOKEN}`,
    `http://controller:7300 ${PIN} ${TOKEN}`,
    `https://controller:99999 ${PIN} ${TOKEN}`,
    `${URL} fingerprint-containing-${TOKEN} ${TOKEN}`,
    `${URL} ${PIN} 'token with spaces ${TOKEN}'`,
    `${URL} ${PIN} ${TOKEN} extra-field`,
    `${URL} ${PIN} ${TOKEN}\0`,
    'x'.repeat(16_385),
  ])('rejects invalid or executable input without echoing credentials (%#)', (input) => {
    expect(() => parseConnection(input)).toThrow('Paste a Gridstator login command');
    try {
      parseConnection(input);
    } catch (error) {
      expect(String(error)).not.toContain(TOKEN);
    }
  });
});

describe('connection prompts', () => {
  it('keeps the pasted credentials in a password input and returns normalized fields', async () => {
    prompts.showQuickPick.mockResolvedValue({ mode: 'paste' });
    prompts.showInputBox.mockResolvedValue(`${URL} ${PIN} ${TOKEN}`);
    await expect(promptConnection()).resolves.toEqual(connection);
    expect(prompts.showInputBox).toHaveBeenCalledWith(expect.objectContaining({ password: true }));
    expect(prompts.showQuickPick).toHaveBeenCalledWith(
      [expect.objectContaining({ mode: 'paste' }), expect.objectContaining({ mode: 'manual' })],
      expect.anything(),
    );
  });

  it('retains manual entry and masks the token', async () => {
    prompts.showQuickPick.mockResolvedValue({ mode: 'manual' });
    prompts.showInputBox
      .mockResolvedValueOnce(URL)
      .mockResolvedValueOnce(PIN)
      .mockResolvedValueOnce(TOKEN);
    await expect(promptConnection()).resolves.toEqual(connection);
    expect(prompts.showInputBox).toHaveBeenLastCalledWith(
      expect.objectContaining({ password: true }),
    );
  });

  it.each([0, 1, 2, 3])('cancels without collecting later fields at step %i', async (step) => {
    prompts.showQuickPick.mockResolvedValue(step === 0 ? undefined : { mode: 'manual' });
    if (step > 1) prompts.showInputBox.mockResolvedValueOnce(URL);
    if (step > 2) prompts.showInputBox.mockResolvedValueOnce(PIN);
    prompts.showInputBox.mockResolvedValueOnce(undefined);
    await expect(promptConnection()).resolves.toBeUndefined();
    expect(prompts.showInputBox).toHaveBeenCalledTimes(step);
  });

  it('cancels pasted entry without parsing', async () => {
    prompts.showQuickPick.mockResolvedValue({ mode: 'paste' });
    prompts.showInputBox.mockResolvedValue(undefined);
    await expect(promptConnection()).resolves.toBeUndefined();
  });
});

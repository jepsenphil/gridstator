import * as vscode from 'vscode';
import { GridstatorError, normalizeFingerprint } from '@gridstator/shared';
import { normalizeControllerUrl } from '@gridstator/client';

export interface ConnectionCredentials {
  url: string;
  fingerprint: string;
  token: string;
}

const PASTE_ERROR =
  'Paste a Gridstator login command, or its HTTPS URL, SHA-256 fingerprint and client token separated by spaces.';
const URL_ERROR = 'Enter an HTTPS controller URL without credentials, a path or query parameters.';
const FINGERPRINT_ERROR = 'Enter the controller’s SHA-256 fingerprint (64 hexadecimal digits).';
const TOKEN_ERROR = 'Enter a client token without spaces.';
const MAX_INPUT = 16_384;

function invalidPaste(): never {
  throw new GridstatorError('BAD_REQUEST', PASTE_ERROR);
}

/** Split shell-quoted fields as data. No substitutions or commands are ever evaluated. */
function fields(input: string): string[] {
  if (!input.trim() || input.length > MAX_INPUT) invalidPaste();
  input = input.replace(/\r\n/g, '\n');
  if (/[\0\r]/.test(input)) invalidPaste();
  const result: string[] = [];
  let field = '';
  let quoted: "'" | '"' | undefined;
  let started = false;
  for (let index = 0; index < input.length; index++) {
    const char = input[index]!;
    if (char === '\\' && quoted !== "'") {
      const next = input[++index];
      if (next === undefined) invalidPaste();
      if (next !== '\n') {
        field += next;
        started = true;
      }
    } else if (quoted) {
      if (char === quoted) quoted = undefined;
      else field += char;
    } else if (char === "'" || char === '"') {
      quoted = char;
      started = true;
    } else if (/\s/.test(char)) {
      if (started) result.push(field);
      field = '';
      started = false;
    } else {
      if (/[;&|<>`$]/.test(char)) invalidPaste();
      field += char;
      started = true;
    }
  }
  if (quoted) invalidPaste();
  if (started) result.push(field);
  return result;
}

function controllerUrl(input: string): string {
  try {
    const url = new URL(input.trim());
    if (
      url.protocol !== 'https:' ||
      !url.hostname ||
      url.username ||
      url.password ||
      (url.pathname && url.pathname !== '/') ||
      url.search ||
      url.hash ||
      /\s/.test(input.trim())
    )
      throw new Error();
    return normalizeControllerUrl(input);
  } catch {
    throw new GridstatorError('BAD_REQUEST', URL_ERROR);
  }
}

function fingerprint(input: string): string {
  try {
    return normalizeFingerprint(input);
  } catch {
    throw new GridstatorError('BAD_REQUEST', FINGERPRINT_ERROR);
  }
}

function clientToken(input: string): string {
  const value = input.trim();
  if (!/^[A-Za-z0-9_-]{1,4096}$/.test(value)) throw new GridstatorError('BAD_REQUEST', TOKEN_ERROR);
  return value;
}

/** Accept the CLI's printed login command or the equivalent three space-separated fields. */
export function parseConnection(input: string): ConnectionCredentials {
  try {
    const parts = fields(input);
    let url: string;
    let pin: string;
    let token: string;
    if (parts.length === 3 && parts[1] !== 'login') {
      [url, pin, token] = [parts[0]!, parts[1]!, parts[2]!];
    } else {
      if (!/(?:^|[/\\])gridstator(?:\.exe)?$/.test(parts[0] ?? '') || parts[1] !== 'login')
        invalidPaste();
      let endpoint: string | undefined;
      const options = new Map<string, string>();
      for (let index = 2; index < parts.length; index++) {
        const part = parts[index]!;
        if (part.startsWith('--')) {
          const equal = part.indexOf('=');
          const key = equal === -1 ? part : part.slice(0, equal);
          const value = equal === -1 ? parts[++index] : part.slice(equal + 1);
          if (
            !['--fingerprint', '--token'].includes(key) ||
            options.has(key) ||
            !value ||
            value.startsWith('--')
          )
            invalidPaste();
          options.set(key, value);
        } else {
          if (endpoint !== undefined) invalidPaste();
          endpoint = part;
        }
      }
      if (!endpoint || !options.get('--fingerprint') || !options.get('--token')) invalidPaste();
      url = endpoint;
      pin = options.get('--fingerprint')!;
      token = options.get('--token')!;
    }
    return { url: controllerUrl(url), fingerprint: fingerprint(pin), token: clientToken(token) };
  } catch {
    // Even normalization helpers may include invalid input in their errors. Never echo a paste
    // containing credentials into validation messages, notifications or extension logs.
    invalidPaste();
  }
}

function validation(validate: (value: string) => unknown): (value: string) => string | undefined {
  return (value) => {
    try {
      validate(value);
      return undefined;
    } catch (error) {
      return error instanceof GridstatorError ? error.message : PASTE_ERROR;
    }
  };
}

/** Uses native VS Code controls so pasted credentials never enter the dashboard webview. */
export async function promptConnection(): Promise<ConnectionCredentials | undefined> {
  const choice = await vscode.window.showQuickPick(
    [
      { label: 'Paste a connection command or string', mode: 'paste' },
      { label: 'Enter connection details manually', mode: 'manual' },
    ],
    { title: 'Gridstator: Connect to a controller', placeHolder: 'How would you like to connect?' },
  );
  if (!choice) return undefined;
  if (choice.mode === 'paste') {
    const input = await vscode.window.showInputBox({
      title: 'Gridstator: Connect to a controller',
      prompt: 'Paste the login command, or the URL, fingerprint and token separated by spaces',
      placeHolder: 'gridstator login https://controller:7300 --fingerprint sha256:… --token gsc_…',
      password: true,
      ignoreFocusOut: true,
      validateInput: validation(parseConnection),
    });
    return input === undefined ? undefined : parseConnection(input);
  }

  const url = await vscode.window.showInputBox({
    title: 'Gridstator: Connect (1/3)',
    prompt: 'HTTPS controller URL',
    placeHolder: 'https://192.168.1.20:7300',
    ignoreFocusOut: true,
    validateInput: validation(controllerUrl),
  });
  if (url === undefined) return undefined;
  const pin = await vscode.window.showInputBox({
    title: 'Gridstator: Connect (2/3)',
    prompt: 'Cluster CA fingerprint printed by the controller',
    placeHolder: 'sha256:…',
    ignoreFocusOut: true,
    validateInput: validation(fingerprint),
  });
  if (pin === undefined) return undefined;
  const token = await vscode.window.showInputBox({
    title: 'Gridstator: Connect (3/3)',
    prompt: 'Client token',
    placeHolder: 'gsc_…',
    password: true,
    ignoreFocusOut: true,
    validateInput: validation(clientToken),
  });
  if (token === undefined) return undefined;
  return { url: controllerUrl(url), fingerprint: fingerprint(pin), token: clientToken(token) };
}

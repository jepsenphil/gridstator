import { homedir } from 'node:os';
import { join } from 'node:path';

function env(name: string): string | undefined {
  const v = process.env[name];
  return v === undefined || v === '' ? undefined : v;
}

/** Base directory for persistent data (controller DB, agent cache). */
export function dataHome(): string {
  if (process.platform === 'win32') {
    return env('LOCALAPPDATA') ?? join(homedir(), 'AppData', 'Local');
  }
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Application Support');
  return env('XDG_DATA_HOME') ?? join(homedir(), '.local', 'share');
}

/** Base directory for user configuration (client credentials). */
export function configHome(): string {
  if (process.platform === 'win32') return env('APPDATA') ?? join(homedir(), 'AppData', 'Roaming');
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Application Support');
  return env('XDG_CONFIG_HOME') ?? join(homedir(), '.config');
}

/** Base directory for disposable caches (snapshot hash cache). */
export function cacheHome(): string {
  if (process.platform === 'win32') {
    return join(env('LOCALAPPDATA') ?? join(homedir(), 'AppData', 'Local'), 'cache');
  }
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Caches');
  return env('XDG_CACHE_HOME') ?? join(homedir(), '.cache');
}

export function defaultControllerDataDir(): string {
  return env('GRIDSTATOR_CONTROLLER_DATA') ?? join(dataHome(), 'gridstator', 'controller');
}

export function defaultAgentDataDir(): string {
  return env('GRIDSTATOR_AGENT_DATA') ?? join(dataHome(), 'gridstator', 'agent');
}

export function defaultClientConfigPath(): string {
  return env('GRIDSTATOR_CONFIG') ?? join(configHome(), 'gridstator', 'client.json');
}

export function defaultCacheDir(): string {
  return env('GRIDSTATOR_CACHE') ?? join(cacheHome(), 'gridstator');
}

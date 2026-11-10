import { join } from 'node:path';

/** Minimal VS Code host primitives; controller/client behavior stays real in integration tests. */
export class EventEmitter<T> {
  private readonly listeners = new Set<(value: T) => void>();
  readonly event = (listener: (value: T) => void) => {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  };
  fire(value: T): void {
    for (const listener of this.listeners) listener(value);
  }
  dispose(): void {
    this.listeners.clear();
  }
}

export const commands = { executeCommand: async () => undefined };

const configuration = new Map<string, unknown>();
export const testHost = {
  progressMessages: [] as string[],
  configure(key: string, value: unknown): void {
    configuration.set(key, value);
  },
  reset(): void {
    configuration.clear();
    this.progressMessages.length = 0;
  },
};

export const workspace = {
  isTrusted: true,
  workspaceFolders: [],
  getConfiguration: () => ({
    get<T>(key: string, fallback: T): T {
      return (configuration.has(key) ? configuration.get(key) : fallback) as T;
    },
    inspect: () => undefined,
    update: async (key: string, value: unknown) => {
      configuration.set(key, value);
    },
  }),
};

export const Uri = {
  file: (path: string) => ({ fsPath: path }),
  joinPath: (uri: { fsPath: string }, ...parts: string[]) => ({
    fsPath: join(uri.fsPath, ...parts),
  }),
};

export const ProgressLocation = { Notification: 15 };
export const ConfigurationTarget = { Global: 1, Workspace: 2 };

export const window = {
  showWarningMessage: async (_message: string, _options: unknown, action: string) => action,
  showOpenDialog: async (): Promise<Array<{ fsPath: string }> | undefined> => undefined,
  async withProgress<T>(
    _options: unknown,
    task: (
      progress: { report(value: { message?: string }): void },
      token: { onCancellationRequested(listener: () => void): { dispose(): void } },
    ) => Promise<T>,
  ): Promise<T> {
    return task(
      {
        report: (value) => {
          if (value.message) testHost.progressMessages.push(value.message);
        },
      },
      { onCancellationRequested: () => ({ dispose: () => undefined }) },
    );
  },
};

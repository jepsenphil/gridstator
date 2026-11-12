import * as vscode from 'vscode';
import { readClientConfig } from '@gridstator/client';
import { GridstatorError } from '@gridstator/shared';
import type { LocalControllerHost } from './messages';
import { nativeClientConfigPath, NativeCancelledError, type NativeCli } from './native';
import { installNativeCli } from './native-install';

const CONNECTION_ENV = [
  'GRIDSTATOR_URL',
  'GRIDSTATOR_TOKEN',
  'GRIDSTATOR_CA_FILE',
  'GRIDSTATOR_CONFIG',
];

export function usesDefaultConnection(
  configuredPath: string,
  environment: NodeJS.ProcessEnv = process.env,
): boolean {
  return !configuredPath && !CONNECTION_ENV.some((key) => environment[key] !== undefined);
}

/** First-run setup must never replace an explicitly selected or broken connection. */
export async function canSetUpController(
  configuredPath: string,
  currentPath: string | undefined,
  environment: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): Promise<boolean> {
  if (platform !== 'linux' && platform !== 'darwin') return false;
  if (!usesDefaultConnection(configuredPath, environment)) return false;
  try {
    return !(await readClientConfig(currentPath));
  } catch {
    return false;
  }
}

export interface SetupResult {
  status: 'connected';
  host: LocalControllerHost;
  workerWaitingForDocker: boolean;
  persistenceWarning?: string;
}

/** All installation and initialization follows an explicit Set up controller action. */
export async function setUpController(
  context: Pick<vscode.ExtensionContext, 'extensionUri'>,
  native: NativeCli,
): Promise<SetupResult> {
  if (!vscode.workspace.isTrusted)
    throw new GridstatorError('FORBIDDEN', 'Trust this workspace before setting up a controller');
  if (CONNECTION_ENV.some((key) => process.env[key] !== undefined))
    throw new GridstatorError('CONFIG', 'Connection environment settings select this connection', {
      hint: 'Use the existing connection or remove the environment overrides before local setup.',
    });
  const configuration = vscode.workspace.getConfiguration('gridstator');
  if (configuration.get<string>('configPath', ''))
    throw new GridstatorError('CONFIG', 'A custom configuration path selects this connection', {
      hint: 'Use the existing connection or clear gridstator.configPath before local setup.',
    });
  return vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: 'Gridstator: setting up a controller on this workspace host',
      cancellable: true,
    },
    async (progress, cancellation) => {
      const abort = new AbortController();
      const subscription = cancellation.onCancellationRequested(() => abort.abort());
      try {
        const report = (message: string) => progress.report({ message });
        report('Checking the native CLI…');
        const installedPath = await native.prepareSetup(
          () => installNativeCli(context.extensionUri.fsPath, report, abort.signal),
          abort.signal,
        );
        // Opening the account's home directory makes its .local/bin fall inside the workspace.
        // Remember this verified installation explicitly so discovery keeps rejecting arbitrary
        // project executables while setup and later windows can still use the installed CLI.
        if (installedPath && !configuration.get<string>('nativeCli.path', ''))
          await configuration.update(
            'nativeCli.path',
            installedPath,
            vscode.ConfigurationTarget.Global,
          );
        report('Creating the controller and starting its background service…');
        return await native.execute<SetupResult>(
          { operation: 'setup', configPath: nativeClientConfigPath() },
          abort.signal,
        );
      } catch (error) {
        if (abort.signal.aborted) throw new NativeCancelledError();
        throw error;
      } finally {
        subscription.dispose();
      }
    },
  );
}

import { useEffect, useRef, useState } from 'react';
import type { DashboardState, WorkerJoinCommand } from '../../src/messages';
import { workerControllerUrlError } from '../../src/connection-command';
import { send } from '../util';
import { Button } from './common';

export function AddWorker({
  data,
  busy,
  result,
  dismiss,
  back,
}: {
  data: DashboardState;
  busy: boolean;
  result: WorkerJoinCommand | null;
  dismiss: () => void;
  back: () => void;
}) {
  const [name, setName] = useState('');
  const nameInput = useRef<HTMLInputElement>(null);
  const resultHeading = useRef<HTMLHeadingElement>(null);
  const [urlOverride, setUrlOverride] = useState<string | null>(null);
  const url = urlOverride ?? data.connection.publicUrl ?? data.connection.url ?? '';
  const urlError = workerControllerUrlError(url);
  const connected = data.connection.status === 'connected';
  const admin = connected && data.connection.role === 'admin';
  const fingerprint = data.connection.caFingerprint;

  useEffect(() => send({ type: 'refreshSettings' }), []);
  useEffect(() => {
    if (result) resultHeading.current?.focus();
    else nameInput.current?.focus();
  }, [result]);

  return (
    <div className="stack worker-join">
      <div>
        <Button kind="ghost" icon="arrow-left" onClick={back}>
          Back to nodes
        </Button>
      </div>
      <header className="form-title">
        <h1>Add a worker</h1>
        <p className="muted">Generate a join command for another computer on your network.</p>
      </header>
      {!admin ? (
        <p className="error-text" role="status">
          {connected
            ? 'Sign in with admin access to generate a worker join command.'
            : 'Connect to a controller before adding a worker.'}
        </p>
      ) : null}
      {result ? (
        <section className="token-secret" aria-label="Worker join command">
          <h2 ref={resultHeading} tabIndex={-1}>
            Join command for {result.name}
          </h2>
          <p role="status">Join command ready. Its secret is only shown in this session.</p>
          <code className="code-block">{result.command}</code>
          <div className="form-actions">
            <Button icon="copy" onClick={() => send({ type: 'copy', text: result.command })}>
              Copy join command
            </Button>
            <Button kind="secondary" onClick={dismiss}>
              Generate another command
            </Button>
          </div>
          <p className="hint">
            Allows one worker to join
            {result.expiresAt ? ` until ${new Date(result.expiresAt).toLocaleString()}` : ''}.{' '}
            Revoke it in Settings → Access tokens if needed.
          </p>
        </section>
      ) : (
        <form
          className="form"
          onSubmit={(event) => {
            event.preventDefault();
            if (admin && fingerprint && name.trim() && !urlError && !busy)
              send({ type: 'createWorkerJoinCommand', name: name.trim(), url: url.trim() });
          }}
        >
          <fieldset className="settings-fieldset form" disabled={!admin || busy}>
            <legend className="sr-only">Generate a worker join command</legend>
            <label className="field">
              <span className="label" id="worker-command-name-label">
                Command name
              </span>
              <input
                ref={nameInput}
                required
                maxLength={64}
                aria-labelledby="worker-command-name-label"
                aria-describedby="worker-command-name-hint"
                value={name}
                placeholder="office-pc"
                onChange={(event) => setName(event.target.value)}
              />
              <span className="hint" id="worker-command-name-hint">
                A name to find this command in Access tokens.
              </span>
            </label>
            <label className="field">
              <span className="label" id="worker-controller-url-label">
                Controller URL for the worker
              </span>
              <input
                required
                type="url"
                value={url}
                placeholder="https://192.168.1.77:7300"
                aria-invalid={Boolean(urlError)}
                aria-labelledby="worker-controller-url-label"
                aria-describedby="worker-controller-url-hint"
                onChange={(event) => setUrlOverride(event.target.value)}
              />
              <span
                id="worker-controller-url-hint"
                className={urlError ? 'error-text hint' : 'hint'}
              >
                {urlError ?? 'Use an address the other computer can reach.'}
              </span>
            </label>
            {!fingerprint && admin ? (
              <p className="error-text" role="status">
                Reconnect to load the controller fingerprint before generating a command.
              </p>
            ) : null}
            <p className="muted">The command expires in 24 hours and allows one worker to join.</p>
            <div>
              <Button
                type="submit"
                icon="terminal"
                busy={busy}
                disabled={!name.trim() || Boolean(urlError) || !fingerprint}
              >
                Generate join command
              </Button>
            </div>
          </fieldset>
        </form>
      )}
      <section className="form worker-join-help" aria-labelledby="worker-join-help">
        <h2 id="worker-join-help">On the other computer</h2>
        <p>
          <strong>Gridstator desktop app:</strong> choose Join a controller and paste the command
          into Worker join command. On Windows, select the WSL2 distribution to run the worker. The
          app starts the worker for you.
        </p>
        <p>
          <strong>Linux, macOS or a WSL terminal:</strong> with the native Gridstator CLI installed,
          run the join command, then start its background service:
        </p>
        <code className="code-block">gridstator agent install-service</code>
        <p className="hint">Docker must be available on the worker to run jobs.</p>
      </section>
    </div>
  );
}

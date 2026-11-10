import { send } from '../util';
import { Button } from './common';

export function GettingStarted({ busy, pending }: { busy: boolean; pending?: boolean }) {
  return (
    <section className="getting-started" aria-labelledby="getting-started-title">
      <h2 id="getting-started-title">Connect your machines</h2>
      <p className="muted">
        Run builds and tests here or across your network. Choose where your controller runs.
      </p>
      <div className="setup-choice">
        <h3>Start on this machine</h3>
        <p>
          Set up a controller and worker on this workspace host. Setup installs Gridstator for your
          account and signs you in automatically.
        </p>
        <p className="muted small">
          The controller keeps running when VS Code closes and starts at OS sign-in. Docker is
          needed to run jobs here.
        </p>
        {pending ? (
          <p role="status">
            Controller setup hasn’t finished. Retry to continue where you left off.
          </p>
        ) : null}
        <Button icon="server" busy={busy} onClick={() => send({ type: 'setupController' })}>
          {busy
            ? 'Setting up controller…'
            : pending
              ? 'Finish controller setup'
              : 'Set up a controller here'}
        </Button>
      </div>
      <div className="setup-choice">
        <h3>Use your existing controller</h3>
        <p>Paste a connection command from your controller, or enter its connection details.</p>
        <Button
          kind="secondary"
          icon="plug"
          disabled={busy}
          onClick={() => send({ type: 'signIn' })}
        >
          Connect to an existing controller
        </Button>
      </div>
      <p className="muted small">
        In Remote SSH and WSL windows, “here” means the remote machine or WSL environment.
      </p>
    </section>
  );
}

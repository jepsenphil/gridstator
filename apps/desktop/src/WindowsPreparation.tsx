import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { desktop, errorMessage } from './api.js';
import type { PrerequisiteRequest, PrerequisiteStatus, Snapshot } from './api.js';

export interface PreparedTarget {
  distribution: string;
  linuxUser?: string;
  onReview: () => void;
}

export function WindowsPreparation({
  snapshot,
  onBack,
  children,
}: {
  snapshot: Snapshot;
  onBack?: () => void;
  children: (target: PreparedTarget) => ReactNode;
}) {
  const [status, setStatus] = useState<PrerequisiteStatus | undefined>(
    snapshot.prerequisites ?? undefined,
  );
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [terms, setTerms] = useState(false);
  const [connect, setConnect] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const [confirmation, setConfirmation] = useState<'restart' | 'delete'>();
  const dialog = useRef<HTMLDialogElement>(null);
  const inFlight = useRef(false);
  const mounted = useRef(true);

  const requestQueue = useRef<Promise<void>>(Promise.resolve());
  const mutationPending = useRef(false);
  const send = useCallback((request: PrerequisiteRequest, quiet = false) => {
    // Polls may be skipped. Explicit user actions wait behind an in-flight inspection.
    if (quiet && (inFlight.current || mutationPending.current)) return Promise.resolve(undefined);
    if (!quiet && mutationPending.current) return Promise.resolve(undefined);
    if (!quiet) {
      mutationPending.current = true;
      setBusy(true);
    }
    const operation = async () => {
      inFlight.current = true;
      try {
        const next = await desktop.prerequisites(request);
        if (mounted.current) {
          setStatus(next);
          setError('');
        }
        return next;
      } catch (reason) {
        if (mounted.current) setError(errorMessage(reason));
        return undefined;
      } finally {
        inFlight.current = false;
        if (!quiet) {
          mutationPending.current = false;
          if (mounted.current) setBusy(false);
        }
      }
    };
    const result = requestQueue.current.then(operation, operation);
    requestQueue.current = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }, []);

  useEffect(() => {
    mounted.current = true;
    void send({ action: 'inspect' });
    return () => {
      mounted.current = false;
    };
  }, [send]);

  useEffect(() => {
    if (status?.phase !== 'running' && status?.phase !== 'restartRequired') return;
    const timer = window.setInterval(() => void send({ action: 'inspect' }, true), 2000);
    return () => window.clearInterval(timer);
  }, [send, status?.phase]);

  useEffect(() => {
    setTerms(false);
  }, [status?.revision, status?.engine]);

  useEffect(() => {
    if (confirmation) dialog.current?.showModal();
  }, [confirmation]);

  function approved(action: PrerequisiteRequest['action']) {
    if (!status) return;
    if (action === 'start' || action === 'retry') setReviewing(false);
    return send({
      action,
      revision: status.revision,
      engine: status.engine ?? undefined,
      distribution: status.distribution,
      confirmed: true,
      acceptDockerTerms: terms,
    });
  }

  async function confirmChange() {
    if (!confirmation) return;
    await approved(confirmation === 'restart' ? 'restart' : 'delete');
    setConfirmation(undefined);
  }

  if (connect && status?.phase === 'ready') {
    return children({
      distribution: status.distribution,
      linuxUser: status.linuxUser ?? undefined,
      onReview: () => setConnect(false),
    });
  }

  const editing = status?.phase === 'review' || status?.phase === 'failed' || reviewing;
  const running = status?.phase === 'running';
  const engineName = status?.engine === 'desktop' ? 'Docker Desktop' : 'Docker Engine';
  const compatible =
    status?.distributions.filter(
      (entry) =>
        entry.version === 2 &&
        entry.state !== 'unavailable' &&
        !entry.name.startsWith('docker-desktop'),
    ) ?? [];

  return (
    <section className="setup preparation" aria-labelledby="preparation-title">
      {onBack && (
        <button className="button quiet" disabled={busy} onClick={onBack}>
          Back to worker
        </button>
      )}
      <h1 id="preparation-title">
        {snapshot.workers.length ? 'Check Windows setup' : 'Put this machine to work'}
      </h1>
      <p className="setup-intro">
        Prepare this machine, then join or create a controller. Your worker runs in WSL2 with
        managed updates.
      </p>

      {error && (
        <div className="notice notice-error" role="alert">
          {error}
        </div>
      )}

      {!status ? (
        <div role="status" className="preparation-status">
          <p>Checking WSL2, Ubuntu and Docker…</p>
          {error && (
            <button className="button" onClick={() => void send({ action: 'inspect' })}>
              Retry detection
            </button>
          )}
        </div>
      ) : !status.supported ? (
        <div className="notice" role="status">
          Automatic runtime setup is available on Windows.
        </div>
      ) : (
        <>
          {status.existingWorker && (
            <div className="notice">
              <strong>Your existing worker is preserved.</strong>
              <p>
                Setup checks {status.distribution} for {status.linuxUser}. Its registration,
                settings and build data stay in place.
              </p>
            </div>
          )}

          {editing && (
            <fieldset disabled={busy} className="preparation-review">
              <fieldset className="setup-choice">
                <legend>Container engine</legend>
                {(['engine', 'desktop'] as const).map((engine) => (
                  <label className={status.engine === engine ? 'chosen' : ''} key={engine}>
                    <input
                      type="radio"
                      name="engine"
                      value={engine}
                      checked={status.engine === engine}
                      disabled={status.existingWorker && status.engine !== engine}
                      onChange={() => void send({ action: 'inspect', engine })}
                    />
                    <span>
                      <strong>
                        {engine === 'engine' ? 'Docker Engine in Ubuntu' : 'Docker Desktop'}
                      </strong>
                      <span>
                        {engine === 'engine'
                          ? 'Runs inside Ubuntu without Docker Desktop.'
                          : 'Uses Docker Desktop through WSL2 integration.'}
                      </span>
                    </span>
                  </label>
                ))}
              </fieldset>
              <label>
                Ubuntu environment
                <select
                  aria-label="Ubuntu environment"
                  value={status.distribution}
                  onChange={(event) =>
                    void send({
                      action: 'inspect',
                      distribution: event.target.value,
                    })
                  }
                >
                  <option value="Gridstator-Ubuntu">Dedicated Gridstator Ubuntu LTS</option>
                  {compatible
                    .filter((entry) => entry.name !== 'Gridstator-Ubuntu')
                    .map((entry) => (
                      <option key={entry.name} value={entry.name}>
                        {entry.name} · existing WSL2
                      </option>
                    ))}
                </select>
                <span className="field-hint">
                  {status.distribution === 'Gridstator-Ubuntu'
                    ? 'Gridstator creates its own Linux account. No Linux password setup is needed.'
                    : 'Setup checks compatibility before changing this distribution.'}
                </span>
              </label>
              <div className="preparation-summary">
                <strong>What setup will do</strong>
                <ul>
                  <li>
                    {status.wslAvailable
                      ? 'Use this machine’s WSL2 installation.'
                      : 'Install WSL2; Windows may request administrator approval and a restart.'}
                  </li>
                  <li>
                    {status.distribution === 'Gridstator-Ubuntu'
                      ? 'Prepare a dedicated Ubuntu LTS environment.'
                      : `Use the selected ${status.distribution} environment.`}
                  </li>
                  <li>
                    {status.existingWorker
                      ? 'Check the existing worker and its Docker connection.'
                      : `Prepare ${engineName} and verify it with a container job.`}
                  </li>
                </ul>
                <p className="field-hint">
                  You choose when to restart. Setup retains progress and resumes after sign-in.
                  Existing workers and other Linux environments are preserved.
                </p>
              </div>
              {status.dockerTermsRequired && (
                <label className="checkbox-line">
                  <input
                    type="checkbox"
                    checked={terms}
                    onChange={(event) => setTerms(event.target.checked)}
                  />
                  <span>
                    I accept the Docker Subscription Service Agreement.
                    <span className="field-hint">
                      Review the agreement and any applicable subscription requirements before
                      continuing.
                    </span>
                  </span>
                </label>
              )}
              {status.engine === 'desktop' && (
                <p className="field-hint">
                  <button
                    className="text-button"
                    type="button"
                    disabled={busy}
                    onClick={() => void send({ action: 'openDockerTerms' })}
                  >
                    Read Docker’s agreement
                  </button>
                </p>
              )}
            </fieldset>
          )}

          <div className="preparation-status" role="status" aria-live="polite">
            <p>{status.message}</p>
          </div>
          {status.error && (
            <div className="notice notice-error" role="alert">
              {status.error}
            </div>
          )}

          {!editing && !(status.phase === 'ready' && status.existingWorker) && (
            <ol className="preparation-steps" aria-label="Preparation progress">
              {status.steps.map((step, index) => (
                <li
                  key={step.id}
                  data-state={step.state}
                  aria-current={step.state === 'running' ? 'step' : undefined}
                >
                  <span className="preparation-step-marker" aria-hidden="true">
                    {step.state === 'done' ? '✓' : index + 1}
                  </span>
                  <span>{step.label}</span>
                  <span className="preparation-step-state">
                    {step.state === 'done'
                      ? 'Complete'
                      : step.state === 'running'
                        ? 'In progress'
                        : step.state === 'blocked'
                          ? 'Needs attention'
                          : 'Waiting'}
                  </span>
                </li>
              ))}
            </ol>
          )}

          {status.manualAction && !editing && (
            <div className="notice preparation-manual">
              <strong>One step needs your attention</strong>
              <p>{status.manualAction}</p>
              {status.engine === 'desktop' && (
                <button
                  className="button"
                  disabled={busy}
                  onClick={() => void send({ action: 'openDocker' })}
                >
                  Open Docker Desktop
                </button>
              )}
            </div>
          )}

          <div className="actions setup-submit">
            {editing && (
              <button
                className="button primary"
                disabled={busy || (status.dockerTermsRequired && !terms)}
                onClick={() => void approved(status.phase === 'failed' ? 'retry' : 'start')}
              >
                {busy
                  ? 'Starting setup…'
                  : status.phase === 'failed'
                    ? 'Retry setup'
                    : status.existingWorker
                      ? 'Check existing setup'
                      : 'Install and prepare'}
              </button>
            )}
            {running && <p className="field-hint">Setup continues if you close this window.</p>}
            {status.phase === 'restartRequired' && status.canRestart && (
              <>
                <button
                  className="button primary"
                  disabled={busy}
                  onClick={() => setConfirmation('restart')}
                >
                  Restart Windows…
                </button>
                <span className="field-hint">You can leave this open and restart later.</span>
              </>
            )}
            {status.phase === 'actionRequired' && !editing && (
              <button
                className="button primary"
                disabled={busy}
                onClick={() => void send({ action: 'verify' })}
              >
                Verify and continue
              </button>
            )}
            {status.phase === 'actionRequired' && !editing && (
              <button className="button" disabled={busy} onClick={() => setReviewing(true)}>
                Change setup choices
              </button>
            )}
            {status.phase === 'ready' && !editing && (
              <button
                className="button primary"
                disabled={busy}
                onClick={() => {
                  if (status.existingWorker && onBack) onBack();
                  else setConnect(true);
                }}
              >
                {status.existingWorker && onBack ? 'Return to worker' : 'Continue to connection'}
              </button>
            )}
            {status.phase === 'ready' && !editing && (
              <button className="button" disabled={busy} onClick={() => setReviewing(true)}>
                Review environment options
              </button>
            )}
          </div>

          {status.canDelete && !running && status.phase !== 'restartRequired' && (
            <details className="native-path">
              <summary>Remove the dedicated environment</summary>
              <p className="field-hint">
                Uninstalling the companion keeps this environment and its build data. You can
                explicitly delete {status.distribution} here. Stop its worker and controller first.
              </p>
              <button
                className="button danger"
                disabled={busy}
                onClick={() => setConfirmation('delete')}
              >
                Delete environment and data…
              </button>
            </details>
          )}
        </>
      )}

      {confirmation && (
        <dialog
          ref={dialog}
          aria-labelledby="preparation-confirm-title"
          onCancel={(event) => {
            if (busy) event.preventDefault();
            else setConfirmation(undefined);
          }}
        >
          <h2 id="preparation-confirm-title">
            {confirmation === 'restart' ? 'Restart Windows?' : 'Delete the dedicated environment?'}
          </h2>
          <p>
            {confirmation === 'restart'
              ? 'Save your work in other applications. Gridstator retains your selected setup and resumes after sign-in.'
              : `Permanently delete ${status?.distribution}, and all data stored inside it, including worker state and local caches. Shared Windows and Docker components stay installed.`}
          </p>
          <div className="actions dialog-actions">
            <button
              className="button"
              autoFocus
              disabled={busy}
              onClick={() => setConfirmation(undefined)}
            >
              Cancel
            </button>
            <button
              className={`button ${confirmation === 'delete' ? 'danger' : 'primary'}`}
              disabled={busy}
              onClick={() => void confirmChange()}
            >
              {confirmation === 'restart' ? 'Restart Windows now' : 'Delete environment and data'}
            </button>
          </div>
        </dialog>
      )}
    </section>
  );
}

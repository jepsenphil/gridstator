import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { desktop, errorMessage } from './api.js';
import type { Budget, Job, Logs, SetupRequest, Snapshot, Worker, WorkerAction } from './api.js';

function Mark() {
  return (
    <svg className="mark" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3" y="3" width="7.5" height="7.5" rx="1.5" />
      <rect x="13.5" y="3" width="7.5" height="7.5" rx="1.5" />
      <rect x="3" y="13.5" width="7.5" height="7.5" rx="1.5" />
      <rect className="mark-accent" x="13.5" y="13.5" width="7.5" height="7.5" rx="1.5" />
    </svg>
  );
}

function Icon({ name }: { name: 'refresh' | 'pause' | 'play' | 'stop' | 'plus' | 'back' }) {
  const paths = {
    refresh: (
      <>
        <path d="M18 8a7 7 0 1 0 1 7" />
        <path d="M18 3v5h-5" />
      </>
    ),
    pause: (
      <>
        <path d="M8 5v14M16 5v14" />
      </>
    ),
    play: <path d="m8 5 11 7-11 7Z" />,
    stop: <rect x="6" y="6" width="12" height="12" rx="1" />,
    plus: <path d="M12 5v14M5 12h14" />,
    back: <path d="m12 5-7 7 7 7M5 12h14" />,
  };
  return (
    <svg
      className="icon"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {paths[name]}
    </svg>
  );
}

function Button({
  children,
  icon,
  kind = 'secondary',
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  icon?: Parameters<typeof Icon>[0]['name'];
  kind?: 'primary' | 'secondary' | 'quiet' | 'danger';
}) {
  return (
    <button className={`button ${kind}`} {...props}>
      {icon && <Icon name={icon} />}
      {children}
    </button>
  );
}

function Notice({ children, error = false }: { children: ReactNode; error?: boolean }) {
  return (
    <div className={`notice ${error ? 'notice-error' : ''}`} role={error ? 'alert' : 'status'}>
      {children}
    </div>
  );
}

function workerState(worker: Worker) {
  if (worker.state === 'unavailable')
    return {
      label: 'Worker unavailable',
      tone: 'warning',
      description: 'Check that its host and account are available, then refresh this worker.',
    };
  if (!worker.running)
    return {
      label: 'Stopped',
      tone: 'neutral',
      description: 'Start this worker when you are ready to accept jobs.',
    };
  if (worker.draining || worker.state === 'draining')
    return {
      label: 'Stopping after jobs finish',
      tone: 'warning',
      description: 'New jobs are blocked. The worker will stop when current jobs finish.',
    };
  if (worker.paused)
    return {
      label: 'Paused locally',
      tone: 'warning',
      description: 'New jobs are blocked until you resume. Current jobs can finish.',
    };
  if (worker.state === 'waitingForDocker')
    return {
      label: 'Waiting for Docker',
      tone: 'warning',
      description: 'Start Docker on this worker’s host. Setup is complete; the worker will retry.',
    };
  if (!worker.connected)
    return {
      label: 'Controller disconnected',
      tone: 'warning',
      description:
        'Local controls and saved limits remain available. The worker will reconnect automatically.',
    };
  if (controllerHold(worker))
    return {
      label: 'Held by controller',
      tone: 'warning',
      description: 'The controller has stopped new assignments. Your local settings still apply.',
    };
  if (worker.state === 'accepting')
    return {
      label: 'Ready for jobs',
      tone: 'success',
      description: 'Accepting work within this machine’s local budget.',
    };
  return {
    label: worker.state === 'updatePending' ? 'Restart pending' : 'Checking worker',
    tone: 'neutral',
    description: 'Waiting for the worker to report its current admission state.',
  };
}

function controllerHold(worker: Worker): string | undefined {
  if (worker.controllerHold) return worker.controllerHold;
  const mode = worker.controllerSettings?.schedulingMode ?? worker.controllerMode;
  return mode && mode !== 'enabled'
    ? `Scheduling ${mode}`
    : worker.state === 'controllerPaused'
      ? 'Scheduling paused'
      : undefined;
}

function timeLabel(value: string | number | undefined): string {
  if (value === undefined) return 'Time unavailable';
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? 'Time unavailable'
    : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function phaseLabel(phase: string): string {
  return phase.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/[_-]/g, ' ');
}

function formText(data: FormData, name: string): string {
  const value = data.get(name);
  return typeof value === 'string' ? value.trim() : '';
}

function Confirmation({
  title,
  children,
  confirm,
  onConfirm,
  onClose,
  busy,
}: {
  title: string;
  children: ReactNode;
  confirm: string;
  onConfirm: () => void;
  onClose: () => void;
  busy: boolean;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  return (
    <dialog
      ref={dialog}
      onCancel={(event) => {
        if (busy) event.preventDefault();
        else onClose();
      }}
      onClose={onClose}
      aria-labelledby="confirm-title"
    >
      <h2 id="confirm-title">{title}</h2>
      <p>{children}</p>
      <div className="actions dialog-actions">
        <Button autoFocus disabled={busy} onClick={onClose}>
          Keep running
        </Button>
        <Button kind="danger" disabled={busy} onClick={onConfirm}>
          {busy ? 'Working…' : confirm}
        </Button>
      </div>
    </dialog>
  );
}

export function App() {
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [workerId, setWorkerId] = useState('');
  const [tab, setTab] = useState<'activity' | 'resources'>('activity');
  const [setup, setSetup] = useState(false);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');
  const busyRef = useRef(false);
  const mutationGeneration = useRef(0);
  const refreshInFlight = useRef(false);
  const [lastUpdated, setLastUpdated] = useState<Date>();
  const [stale, setStale] = useState(false);
  const [confirmation, setConfirmation] = useState<{
    title: string;
    detail: string;
    confirm: string;
    request: WorkerAction;
  }>();

  const accept = useCallback((next: Snapshot) => {
    setSnapshot(next);
    setWorkerId((id) =>
      next.workers.some((worker) => worker.id === id) ? id : (next.workers[0]?.id ?? ''),
    );
    setLastUpdated(new Date());
    setStale(false);
  }, []);

  const refresh = useCallback(
    async (quiet = false) => {
      if (busyRef.current || refreshInFlight.current) return;
      refreshInFlight.current = true;
      const generation = mutationGeneration.current;
      try {
        const next = await desktop.snapshot();
        if (busyRef.current || generation !== mutationGeneration.current) return;
        accept(next);
        if (!quiet) setError('');
      } catch (reason) {
        if (generation === mutationGeneration.current) {
          setError(errorMessage(reason));
          setStale(true);
        }
      } finally {
        refreshInFlight.current = false;
      }
    },
    [accept],
  );

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(true), 4000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  async function perform(label: string, task: () => Promise<Snapshot>): Promise<boolean> {
    if (busyRef.current) return false;
    busyRef.current = true;
    mutationGeneration.current++;
    setBusy(label);
    setError('');
    try {
      accept(await task());
      return true;
    } catch (reason) {
      setError(errorMessage(reason));
      return false;
    } finally {
      busyRef.current = false;
      setBusy('');
    }
  }

  const worker = snapshot?.workers.find((entry) => entry.id === workerId);
  const state = worker && workerState(worker);
  return (
    <div className="app" aria-busy={Boolean(busy)}>
      <header className="app-header">
        <div className="brand">
          <Mark />
          <span>
            Gridstator<span className="brand-subtitle">Worker companion</span>
          </span>
        </div>
        <Button
          kind="quiet"
          icon="refresh"
          disabled={Boolean(busy)}
          onClick={() => void refresh()}
          aria-label="Refresh worker status"
        >
          Refresh
        </Button>
      </header>
      <main>
        {error && (
          <Notice error>
            <strong>Could not complete the request.</strong> {error}{' '}
            <button className="text-button" onClick={() => void refresh()}>
              Retry status
            </button>
          </Notice>
        )}
        {snapshot?.error && <Notice error>{snapshot.error}</Notice>}
        {stale && snapshot && (
          <Notice>Showing the last known worker status. Refresh to check its current state.</Notice>
        )}
        {busy && (
          <div className="busy-message" role="status">
            {busy}
          </div>
        )}
        {!snapshot ? (
          <section className="loading" aria-label="Loading worker status">
            <h1>Connecting to local workers…</h1>
            <div className="skeleton" />
            <div className="skeleton short" />
            {error && (
              <p>
                Open Gridstator from the desktop application to connect to its local worker
                controls.
              </p>
            )}
          </section>
        ) : setup || snapshot.workers.length === 0 ? (
          <Setup
            snapshot={snapshot}
            busy={Boolean(busy)}
            onBack={snapshot.workers.length ? () => setSetup(false) : undefined}
            onSubmit={async (request) => {
              if (
                await perform('Setting up your worker… This can take a few minutes.', () =>
                  desktop.setup(request),
                )
              )
                setSetup(false);
            }}
            onInstall={(request) =>
              void perform('Installing the verified native CLI…', () => desktop.install(request))
            }
            onNativePath={(nativeCliPath) =>
              void perform('Checking the installed native CLI…', () =>
                desktop.settings({ nativeCliPath }),
              )
            }
            onRefresh={() => void refresh()}
          />
        ) : (
          worker && (
            <>
              <div className="worker-toolbar">
                <div className="worker-select">
                  <label htmlFor="worker-select">Worker on this machine</label>
                  <select
                    id="worker-select"
                    value={workerId}
                    disabled={Boolean(busy)}
                    onChange={(event) => setWorkerId(event.target.value)}
                  >
                    {snapshot.workers.map((entry) => (
                      <option value={entry.id} key={entry.id}>
                        {entry.label || entry.name}
                        {entry.distribution ? ` · ${entry.distribution}` : ''}
                      </option>
                    ))}
                  </select>
                </div>
                <Button
                  kind="quiet"
                  icon="plus"
                  disabled={Boolean(busy)}
                  onClick={() => setSetup(true)}
                >
                  Add worker
                </Button>
              </div>
              <section className="worker-overview" aria-labelledby="worker-heading">
                <div className="overview-copy">
                  <div className={`worker-status ${state?.tone}`}>
                    <span className="status-shape" aria-hidden="true" />
                    {state?.label}
                  </div>
                  <h1 id="worker-heading">{worker.name || worker.label}</h1>
                  <p>{state?.description}</p>
                  <div className="connection-detail">
                    <span className={`connection-state ${worker.connected ? 'success' : ''}`}>
                      {worker.connected ? 'Connected' : 'Disconnected'}
                    </span>
                    <span className="controller-url" title={worker.controllerUrl}>
                      {worker.controllerUrl || 'Controller address unavailable'}
                    </span>
                    {worker.distribution && (
                      <span>
                        {worker.distribution}
                        {worker.linuxUser ? ` / ${worker.linuxUser}` : ''}
                      </span>
                    )}
                  </div>
                </div>
                <div className="actions worker-actions">
                  {worker.paused ? (
                    <Button
                      kind="primary"
                      icon="play"
                      disabled={Boolean(busy)}
                      onClick={() =>
                        void perform('Resuming local admissions…', () =>
                          desktop.action({ workerId, action: 'resume' }),
                        )
                      }
                    >
                      Resume worker
                    </Button>
                  ) : (
                    <Button
                      icon="pause"
                      disabled={Boolean(busy) || worker.draining}
                      onClick={() =>
                        void perform('Saving local pause…', () =>
                          desktop.action({ workerId, action: 'pause' }),
                        )
                      }
                    >
                      Pause worker
                    </Button>
                  )}
                  {worker.running ? (
                    <Button
                      icon="stop"
                      disabled={Boolean(busy) || worker.draining}
                      onClick={() =>
                        void perform('Stopping new work and draining jobs…', () =>
                          desktop.action({ workerId, action: 'stop' }),
                        )
                      }
                    >
                      Stop worker
                    </Button>
                  ) : (
                    <Button
                      kind="primary"
                      icon="play"
                      disabled={Boolean(busy)}
                      onClick={() =>
                        void perform('Starting worker…', () =>
                          desktop.action({ workerId, action: 'start' }),
                        )
                      }
                    >
                      Start worker
                    </Button>
                  )}
                </div>
              </section>
              <div className="holds">
                <span>
                  <strong>Local hold</strong>{' '}
                  {worker.paused
                    ? 'Paused until you resume'
                    : worker.draining
                      ? 'Draining to stop'
                      : 'None'}
                </span>
                <span>
                  <strong>Controller hold</strong>{' '}
                  {controllerHold(worker) ||
                    (worker.connected ? 'None' : 'Unknown while disconnected')}
                </span>
                {worker.embedded && (
                  <span>
                    <strong>Controller</strong>{' '}
                    {worker.controllerRunning === false
                      ? 'Stopped'
                      : worker.controllerRunning
                        ? 'Running independently'
                        : 'Managed separately'}
                  </span>
                )}
              </div>
              {worker.error && <Notice error>{worker.error}</Notice>}
              {typeof worker.info === 'string' && worker.info && <Notice>{worker.info}</Notice>}
              <nav className="tabs" aria-label="Worker views">
                <button
                  className={tab === 'activity' ? 'selected' : ''}
                  aria-current={tab === 'activity' ? 'page' : undefined}
                  onClick={() => setTab('activity')}
                >
                  Activity <span className="count">{worker.activeJobs.length}</span>
                </button>
                <button
                  className={tab === 'resources' ? 'selected' : ''}
                  aria-current={tab === 'resources' ? 'page' : undefined}
                  onClick={() => setTab('resources')}
                >
                  Resources & startup
                </button>
              </nav>
              {tab === 'activity' ? (
                <Activity
                  key={worker.id}
                  worker={worker}
                  busy={Boolean(busy)}
                  onCancel={(job) =>
                    setConfirmation({
                      title: `Cancel ${job.name || 'this job'}?`,
                      detail:
                        'This cancels the job running on this worker. Other jobs and the controller keep running.',
                      confirm: 'Cancel job',
                      request: { action: 'cancel', workerId, jobId: job.id, confirmed: true },
                    })
                  }
                />
              ) : (
                <Resources
                  key={`${worker.id}:${worker.generation}`}
                  worker={worker}
                  snapshot={snapshot}
                  busy={Boolean(busy)}
                  onBudget={(budget) =>
                    void perform('Saving local workload budget…', () =>
                      desktop.action({ action: 'setBudget', workerId, budget }),
                    )
                  }
                  onStartup={(startAtSignIn) =>
                    void perform('Saving worker startup preference…', () =>
                      desktop.action({ action: 'setStartup', workerId, startAtSignIn }),
                    )
                  }
                  onCompanionStartup={(startAtSignIn) =>
                    void perform('Saving companion startup preference…', () =>
                      desktop.settings({ startAtSignIn }),
                    )
                  }
                />
              )}
              <footer className="worker-footer">
                <span>
                  {lastUpdated
                    ? `${stale ? 'Last known status' : 'Status checked'} ${lastUpdated.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`
                    : 'Waiting for status'}{' '}
                  · Closing this window keeps your worker running.
                </span>
                {worker.running && (
                  <Button
                    kind="quiet"
                    disabled={Boolean(busy)}
                    onClick={() =>
                      setConfirmation({
                        title: 'Force-stop this worker?',
                        detail: `Cancel ${worker.activeJobs.length ? `all ${worker.activeJobs.length} running job${worker.activeJobs.length === 1 ? '' : 's'}` : 'any running jobs'} and stop this worker immediately. Its controller and other machines keep running.`,
                        confirm: 'Force-stop worker',
                        request: { action: 'forceStop', workerId, confirmed: true },
                      })
                    }
                  >
                    Force-stop…
                  </Button>
                )}
              </footer>
            </>
          )
        )}
      </main>
      {confirmation && (
        <Confirmation
          title={confirmation.title}
          confirm={confirmation.confirm}
          busy={Boolean(busy)}
          onClose={() => {
            if (!busy) setConfirmation(undefined);
          }}
          onConfirm={() => {
            const request = confirmation.request;
            void perform('Applying worker action…', () => desktop.action(request)).then(
              (success) => {
                if (success) setConfirmation(undefined);
              },
            );
          }}
        >
          {confirmation.detail}
        </Confirmation>
      )}
    </div>
  );
}

function Activity({
  worker,
  busy,
  onCancel,
}: {
  worker: Worker;
  busy: boolean;
  onCancel: (job: Job) => void;
}) {
  const [selectedJob, setSelectedJob] = useState<string>();
  const [logs, setLogs] = useState<Logs>();
  const [logError, setLogError] = useState('');
  const [logLoading, setLogLoading] = useState(false);
  const allJobs = [
    ...worker.activeJobs,
    ...worker.recentJobs.filter((job) => !worker.activeJobs.some((active) => active.id === job.id)),
  ];
  const job = allJobs.find((entry) => entry.id === selectedJob);
  useEffect(() => {
    if (!selectedJob) return;
    let cancelled = false;
    let nextSeq: number | undefined;
    let inFlight = false;
    setLogs(undefined);
    setLogLoading(true);
    setLogError('');
    const read = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await desktop.logs({
          workerId: worker.id,
          jobId: selectedJob,
          afterSeq: nextSeq,
          limit: 500,
        });
        if (cancelled) return;
        nextSeq = next.nextSeq;
        setLogs((previous) => ({
          ...next,
          entries: [...(previous?.entries ?? []), ...next.entries].slice(-1500),
        }));
        setLogError('');
      } catch (reason) {
        if (!cancelled) setLogError(errorMessage(reason));
      } finally {
        inFlight = false;
        if (!cancelled) setLogLoading(false);
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), 3000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [selectedJob, worker.id]);

  const rows = (jobs: Job[], active: boolean) =>
    jobs.map((entry) => (
      <li className={`job-row ${entry.id === selectedJob ? 'job-selected' : ''}`} key={entry.id}>
        <button
          className="job-open"
          onClick={() => setSelectedJob(entry.id)}
          aria-label={`Read logs for ${entry.name || entry.id}`}
        >
          <span className="job-name">
            {entry.name || entry.id}
            <span className="job-id">{entry.id}</span>
          </span>
          <span className={`job-phase ${active ? 'active-phase' : ''}`}>
            {phaseLabel(entry.phase)}
          </span>
          <span className="job-time">{timeLabel(entry.startedAt)}</span>
        </button>
        {active && (
          <Button
            kind="quiet"
            disabled={busy}
            onClick={() => onCancel(entry)}
            aria-label={`Cancel ${entry.name || entry.id}`}
          >
            Cancel…
          </Button>
        )}
      </li>
    ));
  return (
    <div className="activity">
      <section aria-labelledby="active-jobs">
        <div className="section-title">
          <h2 id="active-jobs">Running jobs</h2>
          <span className="muted">{worker.activeJobs.length} active</span>
        </div>
        {worker.activeJobs.length ? (
          <ul className="job-list">{rows(worker.activeJobs, true)}</ul>
        ) : (
          <div className="empty-inline">
            <strong>No jobs running</strong>
            <p>
              {worker.paused
                ? 'This worker stays paused until you resume it.'
                : worker.running
                  ? 'Jobs assigned to this worker will appear here.'
                  : 'Start the worker to accept jobs.'}
            </p>
          </div>
        )}
      </section>
      <section aria-labelledby="recent-jobs">
        <div className="section-title">
          <h2 id="recent-jobs">Recent jobs</h2>
          <span className="muted">Saved on this worker</span>
        </div>
        {worker.recentJobs.length ? (
          <ul className="job-list">
            {rows(
              worker.recentJobs.filter(
                (entry) => !worker.activeJobs.some((active) => active.id === entry.id),
              ),
              false,
            )}
          </ul>
        ) : (
          <p className="empty-text">Completed jobs and their saved logs will appear here.</p>
        )}
      </section>
      {selectedJob && (
        <section className="logs-section" aria-labelledby="logs-title">
          <div className="section-title">
            <h2 id="logs-title">Logs · {job?.name || selectedJob}</h2>
            <Button
              kind="quiet"
              onClick={() => setSelectedJob(undefined)}
              aria-label="Close job logs"
            >
              Close logs
            </Button>
          </div>
          <p className="logs-hint">
            Worker output with secrets masked. Showing up to 1,500 loaded entries.
          </p>
          {logError && (
            <Notice error>{logError} Logs retry automatically while this view is open.</Notice>
          )}
          <pre
            className="log-output"
            tabIndex={0}
            aria-label={`Logs for ${job?.name || selectedJob}`}
          >
            {logLoading
              ? 'Loading saved logs…'
              : logs?.entries.length
                ? logs.entries.map((entry) => (
                    <span className={entry.o === 'stderr' ? 'log-stderr' : ''} key={entry.s}>
                      {entry.d}
                      {entry.d.endsWith('\n') ? '' : '\n'}
                    </span>
                  ))
                : 'No saved log output yet.'}
          </pre>
        </section>
      )}
    </div>
  );
}

function Resources({
  worker,
  snapshot,
  busy,
  onBudget,
  onStartup,
  onCompanionStartup,
}: {
  worker: Worker;
  snapshot: Snapshot;
  busy: boolean;
  onBudget: (budget: Budget) => void;
  onStartup: (enabled: boolean) => void;
  onCompanionStartup: (enabled: boolean) => void;
}) {
  const [validation, setValidation] = useState('');
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    const value = (name: string) => formText(data, name);
    const cpus = value('cpus') ? Number(value('cpus')) : null;
    const memoryMb = value('memoryMb') ? Number(value('memoryMb')) : null;
    const maxConcurrency = value('maxConcurrency') ? Number(value('maxConcurrency')) : null;
    if (
      (cpus !== null && (!Number.isFinite(cpus) || cpus <= 0)) ||
      (memoryMb !== null && (!Number.isSafeInteger(memoryMb) || memoryMb <= 0)) ||
      (maxConcurrency !== null && (!Number.isSafeInteger(maxConcurrency) || maxConcurrency <= 0))
    ) {
      setValidation('Enter positive limits. Memory and concurrent jobs must be whole numbers.');
      return;
    }
    setValidation('');
    onBudget({ cpus, memoryMb, maxConcurrency });
  }
  return (
    <div className="settings-layout">
      <section aria-labelledby="budget-title">
        <h2 id="budget-title">Local workload budget</h2>
        <p className="section-description">
          Limit the work this worker accepts. The controller can request less, but cannot raise
          these local limits.
        </p>
        <form className="budget-form" onSubmit={submit}>
          <div className="budget-fields">
            <label>
              CPU capacity
              <span className="input-unit">
                <input
                  name="cpus"
                  type="number"
                  min="0.01"
                  step="0.01"
                  defaultValue={worker.budget.cpus ?? ''}
                  placeholder="No local limit"
                  disabled={busy}
                />
                <span>cores</span>
              </span>
            </label>
            <label>
              Memory capacity
              <span className="input-unit">
                <input
                  name="memoryMb"
                  type="number"
                  min="1"
                  step="1"
                  defaultValue={worker.budget.memoryMb ?? ''}
                  placeholder="No local limit"
                  disabled={busy}
                />
                <span>MB</span>
              </span>
            </label>
            <label>
              Concurrent jobs
              <input
                name="maxConcurrency"
                type="number"
                min="1"
                step="1"
                defaultValue={worker.budget.maxConcurrency ?? ''}
                placeholder="No local limit"
                disabled={busy}
              />
            </label>
          </div>
          <p className="field-hint">Leave a field empty to use no additional local limit.</p>
          {validation && <Notice error>{validation}</Notice>}
          <Button kind="primary" type="submit" disabled={busy}>
            Save local budget
          </Button>
        </form>
        <table className="capacity-table">
          <caption>
            Reported capacity{worker.connected ? '' : ' · controller settings may be stale'}
          </caption>
          <thead>
            <tr>
              <th scope="col">Resource</th>
              <th scope="col">Controller setting</th>
              <th scope="col">Effective admission limit</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <th scope="row">CPU</th>
              <td>
                {worker.controllerSettings?.reserved?.cpus === undefined
                  ? 'Not reported'
                  : `${worker.controllerSettings.reserved.cpus} cores reserved`}
              </td>
              <td>
                {worker.effectiveBudget?.cpus == null
                  ? 'Not reported'
                  : `${worker.effectiveBudget.cpus} cores`}
              </td>
            </tr>
            <tr>
              <th scope="row">Memory</th>
              <td>
                {worker.controllerSettings?.reserved?.memoryMb === undefined
                  ? 'Not reported'
                  : `${worker.controllerSettings.reserved.memoryMb.toLocaleString()} MB reserved`}
              </td>
              <td>
                {worker.effectiveBudget?.memoryMb == null
                  ? 'Not reported'
                  : `${worker.effectiveBudget.memoryMb.toLocaleString()} MB`}
              </td>
            </tr>
            <tr>
              <th scope="row">Jobs</th>
              <td>
                {worker.controllerSettings?.maxConcurrency == null
                  ? 'Not reported'
                  : `${worker.controllerSettings.maxConcurrency} concurrent`}
              </td>
              <td>
                {worker.effectiveBudget?.maxConcurrency == null
                  ? 'Not reported'
                  : `${worker.effectiveBudget.maxConcurrency} concurrent`}
              </td>
            </tr>
          </tbody>
        </table>
        <div className="budget-explanation">
          <strong>These are admission limits, not a machine usage cap.</strong>
          <p>
            Jobs keep their existing per-job limits. Lower limits apply to new work; jobs already
            running can finish above the new budget. Other applications and WSL distributions can
            use additional resources.
          </p>
        </div>
      </section>
      <section aria-labelledby="startup-title">
        <h2 id="startup-title">Startup</h2>
        <label className="checkbox-line">
          <input
            type="checkbox"
            checked={worker.startAtSignIn}
            disabled={busy}
            onChange={(event) => onStartup(event.target.checked)}
          />
          <span>
            Start this worker at OS sign-in
            <span className="field-hint">
              {worker.distribution
                ? `Starts ${worker.distribution} for this worker.`
                : 'Starts this account’s worker service.'}{' '}
              A saved Pause still blocks new jobs.
            </span>
          </span>
        </label>
        <label className="checkbox-line">
          <input
            type="checkbox"
            checked={snapshot.settings.startAtSignIn}
            disabled={busy}
            onChange={(event) => onCompanionStartup(event.target.checked)}
          />
          <span>
            Open the companion at OS sign-in
            <span className="field-hint">
              The tray or menu-bar app shows worker status. Quitting it leaves the worker running.
            </span>
          </span>
        </label>
        <p className="field-hint startup-note">
          Stopping a worker keeps it stopped for this sign-in session. At the next sign-in, its
          startup preference applies.
        </p>
      </section>
    </div>
  );
}

function Setup({
  snapshot,
  busy,
  onBack,
  onSubmit,
  onInstall,
  onNativePath,
  onRefresh,
}: {
  snapshot: Snapshot;
  busy: boolean;
  onBack?: () => void;
  onSubmit: (request: SetupRequest) => Promise<void>;
  onInstall: (request: { distribution?: string; linuxUser?: string }) => void;
  onNativePath: (path: string) => void;
  onRefresh: () => void;
}) {
  const [mode, setMode] = useState<'join' | 'create'>('join');
  const [manual, setManual] = useState(false);
  const [distribution, setDistribution] = useState(
    () =>
      snapshot.wslDistributions.find(
        (entry) => entry.version === 2 && entry.state !== 'unavailable',
      )?.name ?? '',
  );
  const [validation, setValidation] = useState('');
  const windows = snapshot.platform === 'windows';
  const hasWsl2 = snapshot.wslDistributions.some(
    (entry) => entry.version === 2 && entry.state !== 'unavailable',
  );
  const blocked = windows && (!hasWsl2 || !distribution);
  const form = useRef<HTMLFormElement>(null);
  const nativePath = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (
      !snapshot.wslDistributions.some(
        (entry) =>
          entry.name === distribution && entry.version === 2 && entry.state !== 'unavailable',
      )
    ) {
      setDistribution(
        snapshot.wslDistributions.find(
          (entry) => entry.version === 2 && entry.state !== 'unavailable',
        )?.name ?? '',
      );
    }
  }, [distribution, snapshot.wslDistributions]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    const value = (name: string) => formText(data, name);
    const request: SetupRequest = {
      mode,
      ...(windows ? { distribution, linuxUser: value('linuxUser') || undefined } : {}),
      name: value('name') || undefined,
    };
    if (mode === 'join') {
      if (manual) {
        request.controllerUrl = value('controllerUrl');
        request.caFingerprint = value('caFingerprint');
        request.token = value('token');
        if (!request.controllerUrl || !request.caFingerprint || !request.token) {
          setValidation('Enter the controller URL, CA fingerprint and worker join token.');
          return;
        }
        try {
          const url = new URL(request.controllerUrl);
          if (url.protocol !== 'https:' || url.username || url.password) throw new Error();
        } catch {
          setValidation('Use an HTTPS controller URL without a username or password.');
          return;
        }
      } else {
        request.connection = value('connection');
        if (!request.connection) {
          setValidation('Paste the worker join command from your controller.');
          return;
        }
      }
    }
    setValidation('');
    // Keep credentials only in this transient request. Clear sensitive DOM fields before invoking.
    for (const name of ['connection', 'token']) {
      const field = event.currentTarget.elements.namedItem(name);
      if (field instanceof HTMLInputElement) field.value = '';
    }
    await onSubmit(request);
  }

  return (
    <section className="setup" aria-labelledby="setup-title">
      {onBack && (
        <Button kind="quiet" icon="back" disabled={busy} onClick={onBack}>
          Back to worker
        </Button>
      )}
      <h1 id="setup-title">
        {snapshot.workers.length ? 'Add a local worker' : 'Put this machine to work'}
      </h1>
      <p className="setup-intro">
        Join an existing Gridstator controller, or create one here with a local worker. You stay in
        control of this machine’s workload.
      </p>
      {windows && !hasWsl2 && (
        <Notice>
          <strong>A WSL2 distribution is needed.</strong>
          <p>
            Install WSL from Windows Terminal with <code>wsl --install</code>, then finish the
            distribution’s first-run account setup. Restart Windows if prompted.
          </p>
          {snapshot.wslDistributions.some((entry) => entry.version === 1) && (
            <p>
              Your WSL1 distributions cannot run this worker. Convert a distribution to WSL2
              yourself before selecting it.
            </p>
          )}
          <Button disabled={busy} icon="refresh" onClick={onRefresh}>
            Check WSL again
          </Button>
        </Notice>
      )}
      <form ref={form} onSubmit={(event) => void submit(event)}>
        <fieldset disabled={busy} className="setup-fields">
          {windows && (
            <div className="host-fields">
              <label>
                WSL2 distribution
                <select
                  aria-label="WSL2 distribution"
                  value={distribution}
                  onChange={(event) => setDistribution(event.target.value)}
                  required
                >
                  <option value="">Choose a distribution</option>
                  {snapshot.wslDistributions.map((entry) => (
                    <option
                      value={entry.name}
                      key={entry.name}
                      disabled={entry.version !== 2 || entry.state === 'unavailable'}
                    >
                      {entry.name} · {entry.version === 1 ? 'WSL1 — unsupported' : entry.state}
                    </option>
                  ))}
                </select>
                <span className="field-hint">
                  Setup runs inside this distribution. Other distributions are left alone.
                </span>
              </label>
              <label>
                Linux account <span className="optional">(optional)</span>
                <input
                  name="linuxUser"
                  aria-label="Linux account"
                  autoComplete="off"
                  placeholder="Distribution’s default account"
                />
                <span className="field-hint">Use the account that will own this worker.</span>
              </label>
            </div>
          )}
          <fieldset className="setup-choice">
            <legend>How would you like to connect?</legend>
            <label className={mode === 'join' ? 'chosen' : ''}>
              <input
                type="radio"
                name="mode"
                checked={mode === 'join'}
                onChange={() => {
                  setMode('join');
                  setValidation('');
                }}
              />
              <span>
                <strong>Join a controller</strong>
                <span>Connect this worker to your existing cluster.</span>
              </span>
            </label>
            <label className={mode === 'create' ? 'chosen' : ''}>
              <input
                type="radio"
                name="mode"
                checked={mode === 'create'}
                onChange={() => {
                  setMode('create');
                  setValidation('');
                }}
              />
              <span>
                <strong>Create a controller here</strong>
                <span>Set up a controller and worker on this host.</span>
              </span>
            </label>
          </fieldset>
          {mode === 'join' ? (
            <div className="join-fields">
              {manual ? (
                <>
                  <label>
                    Controller URL
                    <input
                      name="controllerUrl"
                      type="url"
                      autoComplete="off"
                      placeholder="https://controller.example:7443"
                      required
                    />
                  </label>
                  <label>
                    Controller CA fingerprint
                    <input
                      name="caFingerprint"
                      aria-label="Controller CA fingerprint"
                      autoComplete="off"
                      spellCheck={false}
                      required
                    />
                    <span className="field-hint">
                      Verify this fingerprint with the controller owner.
                    </span>
                  </label>
                  <label>
                    Worker join token
                    <input
                      name="token"
                      aria-label="Worker join token"
                      type="password"
                      autoComplete="new-password"
                      required
                    />
                    <span className="field-hint">
                      Use a worker join token, not a client login token.
                    </span>
                  </label>
                </>
              ) : (
                <label>
                  Worker join command
                  <input
                    name="connection"
                    aria-label="Worker join command"
                    type="password"
                    autoComplete="new-password"
                    spellCheck={false}
                    placeholder="Paste gridstator agent join …"
                    required
                  />
                  <span className="field-hint">
                    Copied from your controller. Its URL, CA fingerprint and join token are
                    validated; pasted commands are never executed.
                  </span>
                </label>
              )}
              <button
                className="text-button"
                type="button"
                onClick={() => {
                  setManual(!manual);
                  setValidation('');
                }}
              >
                {manual ? 'Paste a join command instead' : 'Enter connection details manually'}
              </button>
            </div>
          ) : (
            <Notice>
              The controller will run as a background user service on{' '}
              {windows ? distribution || 'your selected WSL2 host' : 'this host'}. It stays running
              when you close the companion or stop its worker. If Docker is unavailable, the worker
              waits for it.
            </Notice>
          )}
          <label className="worker-name">
            Worker name <span className="optional">(optional)</span>
            <input
              name="name"
              maxLength={120}
              autoComplete="off"
              placeholder="Use this machine’s name"
            />
          </label>
          {!snapshot.nativeAvailable && (
            <div className="install-note">
              <strong>The native CLI is needed for setup.</strong>
              <p>Setup installs and verifies the native CLI for this account before continuing.</p>
              <Button
                type="button"
                disabled={busy || blocked}
                onClick={() => {
                  const data = new FormData(form.current!);
                  onInstall({
                    ...(windows
                      ? {
                          distribution,
                          linuxUser: formText(data, 'linuxUser') || undefined,
                        }
                      : {}),
                  });
                }}
              >
                Install verified CLI now
              </Button>
            </div>
          )}
          {!windows && (
            <details className="native-path" open={!snapshot.nativeAvailable}>
              <summary>Use an installed native CLI</summary>
              <label>
                Native CLI path
                <input
                  ref={nativePath}
                  name="nativeCliPath"
                  defaultValue={snapshot.settings.nativeCliPath ?? ''}
                  placeholder="/absolute/path/to/gridstator"
                  autoComplete="off"
                  spellCheck={false}
                />
              </label>
              <p className="field-hint">
                Choose an existing compatible native executable. Gridstator checks its identity and
                worker-control support before using it.
              </p>
              <div className="actions">
                <Button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    const path = nativePath.current?.value.trim() ?? '';
                    if (!path.startsWith('/')) {
                      setValidation('Enter an absolute path to the native Gridstator executable.');
                      return;
                    }
                    setValidation('');
                    onNativePath(path);
                  }}
                >
                  Use this CLI
                </Button>
                <Button
                  type="button"
                  kind="quiet"
                  disabled={busy}
                  onClick={() => {
                    if (nativePath.current) nativePath.current.value = '';
                    setValidation('');
                    onNativePath('');
                  }}
                >
                  Use automatic discovery
                </Button>
              </div>
            </details>
          )}
          <p className="field-hint">
            The worker starts automatically at OS sign-in by default. You can change this or pause
            it after setup.
          </p>
          {validation && <Notice error>{validation}</Notice>}
          <div className="actions setup-submit">
            <Button kind="primary" type="submit" disabled={busy || blocked}>
              {busy
                ? 'Setting up…'
                : mode === 'join'
                  ? 'Join controller'
                  : 'Create controller & worker'}
            </Button>
          </div>
        </fieldset>
      </form>
    </section>
  );
}

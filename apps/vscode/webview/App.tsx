import { useEffect, useReducer } from 'react';
import type { ToWebview, View } from '../src/messages';
import { Button, Icon, isActive } from './components/common';
import { GroupDetail } from './components/GroupDetail';
import { JobDetail } from './components/JobDetail';
import { Jobs } from './components/Jobs';
import { Nodes } from './components/Nodes';
import { Overview } from './components/Overview';
import { Run } from './components/Run';
import { Settings } from './components/Settings';
import { GettingStarted } from './components/GettingStarted';
import { AddWorker } from './components/AddWorker';
import { initialState, reducer, type UiState } from './store';
import { cx, loadUiState, saveUiState, send } from './util';

const TABS: Array<{ view: View; label: string; icon: string }> = [
  { view: 'overview', label: 'Overview', icon: 'dashboard' },
  { view: 'jobs', label: 'Jobs', icon: 'list-unordered' },
  { view: 'run', label: 'Run', icon: 'play' },
  { view: 'nodes', label: 'Nodes', icon: 'server' },
  { view: 'settings', label: 'Settings', icon: 'settings-gear' },
];

export function App() {
  const [state, dispatch] = useReducer(reducer, initialState, (s): UiState => {
    const saved = loadUiState<Pick<UiState, 'view' | 'jobId' | 'itemId'>>();
    return saved ? { ...s, ...saved } : s;
  });

  useEffect(() => {
    const onMessage = (e: MessageEvent<ToWebview>) => dispatch(e.data);
    window.addEventListener('message', onMessage);
    send({ type: 'ready' });
    return () => window.removeEventListener('message', onMessage);
  }, []);

  useEffect(() => {
    saveUiState({ view: state.view, jobId: state.jobId, itemId: state.itemId });
  }, [state.view, state.jobId, state.itemId]);

  useEffect(() => {
    if (state.toasts.length === 0) return;
    const t = setTimeout(() => dispatch({ type: 'dismissToast', id: state.toasts[0]!.id }), 5000);
    return () => clearTimeout(t);
  }, [state.toasts]);

  const data = state.data;
  if (!data) {
    return (
      <div className="boot">
        <Icon name="loading" spin /> Connecting…
      </div>
    );
  }
  const c = data.connection;
  const firstRun = (c.status === 'signedOut' || c.status === 'error') && c.canSetUpController;
  const running = data.jobs.filter((j) => isActive(j.state)).length;

  return (
    <div className="app">
      <header className="app-header">
        <div className="brand">
          <span className="logo" aria-hidden="true">
            <svg viewBox="0 0 16 16" width="18" height="18">
              <rect x="1" y="1" width="6" height="6" rx="1.2" />
              <rect x="9" y="1" width="6" height="6" rx="1.2" />
              <rect x="1" y="9" width="6" height="6" rx="1.2" />
              <rect x="9" y="9" width="6" height="6" rx="1.2" className="logo-accent" />
            </svg>
          </span>
          <span className="brand-name">Gridstator</span>
        </div>
        <nav className="tabs" role="tablist">
          {TABS.map((t) => (
            <button
              key={t.view}
              role="tab"
              aria-selected={
                state.view === t.view ||
                (t.view === 'jobs' && (state.view === 'job' || state.view === 'group')) ||
                (t.view === 'nodes' && state.view === 'addWorker')
              }
              className={cx(
                'tab',
                (state.view === t.view ||
                  (t.view === 'jobs' && (state.view === 'job' || state.view === 'group')) ||
                  (t.view === 'nodes' && state.view === 'addWorker')) &&
                  'tab-on',
              )}
              onClick={() => dispatch({ type: 'go', view: t.view })}
            >
              <Icon name={t.icon} /> {t.label}
              {t.view === 'jobs' && running ? <span className="tab-badge">{running}</span> : null}
            </button>
          ))}
        </nav>
        <div className="conn">
          <span
            className={cx(
              'dot',
              firstRun
                ? 'dot-muted'
                : c.status === 'connected'
                  ? 'dot-ok'
                  : c.status === 'connecting'
                    ? 'dot-warn'
                    : 'dot-bad',
            )}
          />
          <div
            className="conn-label"
            title={`${c.url ?? ''}${c.user ? ` · ${c.user} (${c.role ?? ''})` : ''}`}
          >
            <span className="small ellipsis controller-name">
              {c.controller
                ? `Controller · ${c.controller.hostname}`
                : c.status === 'connected'
                  ? `Controller · ${c.url?.replace('https://', '')}`
                  : 'Gridstator controller'}
            </span>
            <span className="small muted ellipsis">
              {c.status === 'connected'
                ? `${c.user} · v${c.version}`
                : c.status === 'connecting'
                  ? 'connecting…'
                  : c.status === 'signedOut'
                    ? firstRun
                      ? 'ready to set up'
                      : 'signed out'
                    : 'offline'}
            </span>
          </div>
          <Button
            kind="ghost"
            icon="refresh"
            title="Refresh"
            onClick={() => send({ type: 'refresh' })}
          />
        </div>
      </header>

      {c.status !== 'connected' && !firstRun ? (
        <div className={cx('banner', c.status === 'connecting' ? 'banner-info' : 'banner-error')}>
          <Icon
            name={c.status === 'connecting' ? 'loading' : 'plug'}
            spin={c.status === 'connecting'}
          />
          <span>
            {c.status === 'signedOut'
              ? 'Not connected to a Gridstator controller.'
              : c.status === 'connecting'
                ? 'Connecting to the controller…'
                : `Cannot reach the controller: ${c.error ?? 'unknown error'}`}
          </span>
          <span className="spacer" />
          {c.status !== 'connecting' ? (
            <>
              <Button kind="secondary" icon="refresh" onClick={() => send({ type: 'refresh' })}>
                Retry
              </Button>
              {c.canStartLocalController ? (
                <Button
                  icon="play"
                  busy={state.busy['settings:host']}
                  onClick={() => send({ type: 'hostService', action: 'start' })}
                >
                  Start controller
                </Button>
              ) : null}
              {c.canRestoreLocalAccess ? (
                <Button icon="key" onClick={() => send({ type: 'restoreLocalAccess' })}>
                  Restore local access
                </Button>
              ) : (
                <Button icon="sign-in" onClick={() => send({ type: 'signIn' })}>
                  Sign in
                </Button>
              )}
            </>
          ) : null}
        </div>
      ) : null}

      <main className="content">
        {state.view === 'overview' ? (
          firstRun ? (
            <GettingStarted busy={Boolean(state.busy['setup'])} pending={c.setupPending} />
          ) : (
            <Overview data={data} dispatch={dispatch} />
          )
        ) : null}
        {state.view === 'jobs' ? <Jobs data={data} dispatch={dispatch} busy={state.busy} /> : null}
        {state.view === 'job' ? (
          <JobDetail
            data={data}
            jobId={state.jobId}
            logs={state.jobId ? state.logs[state.jobId] : undefined}
            events={state.jobId ? state.events[state.jobId] : undefined}
            busy={state.busy}
            dispatch={dispatch}
          />
        ) : null}
        {state.view === 'group' ? (
          <GroupDetail
            data={data}
            groupId={state.groupId}
            tests={state.groupId ? state.groupTests[state.groupId] : undefined}
            busy={state.busy}
            dispatch={dispatch}
          />
        ) : null}
        {state.view === 'run' ? (
          <Run
            data={data}
            selected={state.itemId}
            busy={state.busy}
            onSelect={(id) => dispatch({ type: 'go', view: 'run', itemId: id })}
          />
        ) : null}
        {state.view === 'nodes' ? (
          <Nodes data={data} focus={state.nodeId} busy={state.busy} dispatch={dispatch} />
        ) : null}
        {state.view === 'addWorker' ? (
          <AddWorker
            key={c.clusterId}
            data={data}
            busy={Boolean(state.busy['settings:tokens'])}
            result={state.workerJoinCommand}
            dismiss={() => dispatch({ type: 'dismissWorkerJoinCommand' })}
            back={() => dispatch({ type: 'go', view: 'nodes' })}
          />
        ) : null}
        {state.view === 'settings' ? (
          <Settings
            data={data}
            busy={state.busy}
            createdToken={state.createdToken}
            dismissToken={() => dispatch({ type: 'dismissToken' })}
          />
        ) : null}
      </main>

      <div className="toasts" role="status" aria-live="polite">
        {state.toasts.map((t) => (
          <div key={t.id} className={cx('toast', `toast-${t.level}`)}>
            <Icon name={t.level === 'error' ? 'error' : 'info'} />
            <span>{t.message}</span>
            <Button
              kind="ghost"
              icon="close"
              title="Dismiss"
              onClick={() => dispatch({ type: 'dismissToast', id: t.id })}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

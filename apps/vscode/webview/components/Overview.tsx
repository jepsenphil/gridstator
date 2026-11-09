import type { Dispatch } from 'react';
import type { DashboardState } from '../../src/messages';
import type { Action } from '../store';
import { ago, formatBytes, formatDuration } from '../util';
import {
  Button,
  Card,
  Empty,
  Icon,
  KindIcon,
  Meter,
  StateIcon,
  isActive,
  nodeStatus,
} from './common';

export function Overview({ data, dispatch }: { data: DashboardState; dispatch: Dispatch<Action> }) {
  const online = data.nodes.filter((n) => n.status === 'online');
  const running = data.jobs.filter((j) => isActive(j.state));
  const queued = data.jobs.filter((j) => j.state === 'queued');
  const dayAgo = Date.now() - 86_400_000;
  const recent = data.jobs.filter((j) => (j.finishedAt ?? 0) > dayAgo);
  const failed = recent.filter((j) => j.state === 'failed' || j.state === 'lost');
  const succeeded = recent.filter((j) => j.state === 'succeeded');
  const totalCpus = online.reduce((n, x) => n + (x.info?.cpus ?? 0), 0);
  const totalMem = online.reduce((n, x) => n + (x.info?.memoryTotalMb ?? 0), 0);
  return (
    <div className="stack">
      <div className="tiles">
        <button className="tile" onClick={() => dispatch({ type: 'go', view: 'nodes' })}>
          <span className="tile-value">
            {online.length}
            <small>/{data.nodes.length}</small>
          </span>
          <span className="tile-label">nodes online</span>
          <span className="tile-sub">
            {totalCpus} CPU · {formatBytes(totalMem * 1024 * 1024)}
          </span>
        </button>
        <button className="tile" onClick={() => dispatch({ type: 'go', view: 'jobs' })}>
          <span className="tile-value accent">{running.length}</span>
          <span className="tile-label">running</span>
          <span className="tile-sub">{queued.length} queued</span>
        </button>
        <button className="tile" onClick={() => dispatch({ type: 'go', view: 'jobs' })}>
          <span className="tile-value ok">{succeeded.length}</span>
          <span className="tile-label">succeeded</span>
          <span className="tile-sub">last 24 hours</span>
        </button>
        <button className="tile" onClick={() => dispatch({ type: 'go', view: 'jobs' })}>
          <span className={failed.length ? 'tile-value bad' : 'tile-value'}>{failed.length}</span>
          <span className="tile-label">failed or lost</span>
          <span className="tile-sub">last 24 hours</span>
        </button>
      </div>

      <div className="grid-2">
        <Card
          title="Nodes"
          actions={
            <Button
              kind="ghost"
              icon="gear"
              onClick={() => dispatch({ type: 'go', view: 'nodes' })}
            >
              Manage
            </Button>
          }
        >
          {data.nodes.length === 0 ? (
            <Empty icon="server" title="No worker nodes yet">
              Run <code>gridstator token create --join</code> on the controller, then{' '}
              <code>gridstator agent join …</code> on a worker.
            </Empty>
          ) : (
            <ul className="node-list">
              {data.nodes.map((n) => {
                const st = nodeStatus(n);
                const memUsed =
                  n.info && n.metrics ? n.info.memoryTotalMb - n.metrics.memoryAvailableMb : 0;
                return (
                  <li key={n.id}>
                    <button
                      className="node-row"
                      onClick={() => dispatch({ type: 'go', view: 'nodes', nodeId: n.id })}
                    >
                      <span className={`dot dot-${st.className}`} />
                      <span className="node-name">{n.name}</span>
                      <span className="muted small">{st.label}</span>
                      <span className="node-meters">
                        <span className="meter-label">CPU</span>
                        <Meter
                          value={n.metrics?.cpuPercent ?? 0}
                          max={100}
                          label={`${n.name} CPU`}
                        />
                        <span className="meter-label">RAM</span>
                        <Meter
                          value={memUsed}
                          max={n.info?.memoryTotalMb ?? 1}
                          label={`${n.name} memory`}
                        />
                      </span>
                      <span className="small">
                        {n.activeJobs.length}/{n.settings.maxConcurrency} jobs
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </Card>

        <Card
          title="Recent jobs"
          actions={
            <Button kind="ghost" icon="play" onClick={() => dispatch({ type: 'go', view: 'run' })}>
              Run…
            </Button>
          }
        >
          {data.jobs.length === 0 ? (
            <Empty icon="history" title="No jobs yet">
              Pick a task, workflow, script or Dockerfile in the Run tab.
            </Empty>
          ) : (
            <ul className="job-mini-list">
              {data.jobs.slice(0, 10).map((j) => (
                <li key={j.id}>
                  <button
                    className="job-mini"
                    onClick={() => dispatch({ type: 'go', view: 'job', jobId: j.id })}
                  >
                    <StateIcon state={j.state} />
                    <KindIcon kind={j.kind} />
                    <span className="ellipsis">{j.name}</span>
                    <span className="muted small">
                      {j.nodeName ?? (j.state === 'queued' ? 'queued' : '')} ·{' '}
                      {j.startedAt
                        ? formatDuration((j.finishedAt ?? Date.now()) - j.startedAt)
                        : ago(j.createdAt)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
      {queued.some((j) => j.stateReason) ? (
        <Card
          title={
            <>
              <Icon name="clock" /> Waiting
            </>
          }
        >
          <ul className="plain">
            {queued.map((j) => (
              <li key={j.id}>
                <button
                  className="link"
                  onClick={() => dispatch({ type: 'go', view: 'job', jobId: j.id })}
                >
                  {j.name}
                </button>{' '}
                <span className="muted">
                  — #{j.queuePosition} · {j.stateReason}
                </span>
              </li>
            ))}
          </ul>
        </Card>
      ) : null}
    </div>
  );
}

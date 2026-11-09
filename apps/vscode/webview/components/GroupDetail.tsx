import { useEffect, useState, type Dispatch } from 'react';
import type { JobGroup } from '@gridstator/protocol';
import type { TestSummary } from '@gridstator/reports';
import type { DashboardState } from '../../src/messages';
import type { Action } from '../store';
import { cx, formatDuration, send } from '../util';
import { Button, Card, Empty, Icon, StateIcon, isActive } from './common';

const GROUP_ICON: Record<JobGroup['state'], { icon: string; className: string }> = {
  queued: { icon: 'clock', className: 'state-queued' },
  running: { icon: 'sync', className: 'state-running' },
  succeeded: { icon: 'pass-filled', className: 'state-succeeded' },
  failed: { icon: 'error', className: 'state-failed' },
  cancelled: { icon: 'circle-slash', className: 'state-cancelled' },
};

export function GroupStateBadge({ state }: { state: JobGroup['state'] }) {
  const g = GROUP_ICON[state];
  return (
    <span className={cx('badge', `badge-${state}`)}>
      <Icon name={g.icon} spin={state === 'running'} /> {state}
    </span>
  );
}

export function GroupStateIcon({ state }: { state: JobGroup['state'] }) {
  const g = GROUP_ICON[state];
  return <Icon name={g.icon} spin={state === 'running'} className={g.className} title={state} />;
}

/** Bars from each shard's start to its end on a shared time axis: parallelism at a glance. */
function Timeline({ group, now }: { group: JobGroup; now: number }) {
  const starts = group.jobs.map((j) => j.startedAt).filter((t): t is number => t !== null);
  if (starts.length === 0) return <p className="muted small">No shard has started yet.</p>;
  const t0 = Math.min(...starts);
  const t1 = Math.max(...group.jobs.map((j) => j.finishedAt ?? (j.startedAt ? now : t0)));
  const span = Math.max(1, t1 - t0);
  return (
    <div className="timeline" aria-label="Shard timeline">
      {group.jobs.map((j) => {
        const left = j.startedAt ? ((j.startedAt - t0) / span) * 100 : 0;
        const width = j.startedAt ? (((j.finishedAt ?? now) - j.startedAt) / span) * 100 : 0;
        return (
          <div key={j.id} className="timeline-row">
            <span className="small mono">
              {j.shardIndex}/{j.shardTotal}
            </span>
            <div className="timeline-track">
              {j.startedAt ? (
                <div
                  className={cx('timeline-bar', `bar-${j.state}`)}
                  style={{ left: `${left}%`, width: `${Math.max(width, 0.8)}%` }}
                  title={`${j.nodeName ?? ''} ${formatDuration((j.finishedAt ?? now) - j.startedAt)}`}
                />
              ) : null}
            </div>
            <span className="small muted">{j.nodeName ?? '–'}</span>
          </div>
        );
      })}
      <div className="timeline-axis small muted">
        <span>0s</span>
        <span>{formatDuration(span)}</span>
      </div>
    </div>
  );
}

function Tests({ summary }: { summary: TestSummary | null | undefined }) {
  if (summary === undefined) {
    return (
      <p className="muted small">
        <Icon name="loading" spin /> Reading test reports…
      </p>
    );
  }
  if (summary === null)
    return (
      <p className="muted small">
        No JUnit reports in this group's artifacts. Add `junit:` to the task in gridstator.yml.
      </p>
    );
  return (
    <div className="stack">
      <div className="test-totals">
        <span className="tile-value">{summary.total}</span> tests ·
        <span className="ok-text"> {summary.passed} passed</span> ·
        <span className={summary.failed ? 'error-text' : ''}> {summary.failed} failed</span> ·{' '}
        <span className="muted">{summary.skipped} skipped</span>
        <span className="muted small"> (combined from every shard)</span>
      </div>
      {summary.failures.length ? (
        <ul className="plain">
          {summary.failures.map((f, i) => (
            <li key={i}>
              <Icon name="error" className="state-failed" /> {f.suite} › <strong>{f.name}</strong>
              {f.shard ? <span className="pill">shard {f.shard}</span> : null}
              {f.message ? <div className="muted small">{f.message}</div> : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function GroupDetail({
  data,
  groupId,
  tests,
  busy,
  dispatch,
}: {
  data: DashboardState;
  groupId: string | null;
  tests: TestSummary | null | undefined;
  busy: Record<string, boolean>;
  dispatch: Dispatch<Action>;
}) {
  const group = data.groups.find((g) => g.id === groupId);
  const [now, setNow] = useState(Date.now());
  const active = group ? group.state === 'running' || group.state === 'queued' : false;
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [active]);
  useEffect(() => {
    if (group && !active) send({ type: 'requestGroupTests', groupId: group.id });
  }, [group?.id, active, group?.finishedAt]);
  if (!group) {
    return (
      <Empty icon="search" title="Group not found">
        <button className="link" onClick={() => dispatch({ type: 'go', view: 'jobs' })}>
          Back to jobs
        </button>
      </Empty>
    );
  }
  const failed = group.jobs.filter((j) => j.state !== 'succeeded');
  return (
    <div className="stack">
      <div className="job-header">
        <Button
          kind="ghost"
          icon="arrow-left"
          title="Back to jobs"
          onClick={() => dispatch({ type: 'go', view: 'jobs' })}
        />
        <Icon name="layers" />
        <h2 className="ellipsis" title={group.name}>
          {group.name}
        </h2>
        <GroupStateBadge state={group.state} />
        <span className="muted">
          {group.shardTotal} shards · {group.nodes.join(', ') || 'not started'}
          {group.startedAt
            ? ` · ${formatDuration((group.finishedAt ?? now) - group.startedAt)}`
            : ''}
        </span>
        <span className="spacer" />
        {active ? (
          <Button
            kind="danger"
            icon="debug-stop"
            busy={busy[`cancelGroup:${group.id}`]}
            onClick={() => send({ type: 'cancelGroup', groupId: group.id })}
          >
            Cancel all shards
          </Button>
        ) : failed.length ? (
          <Button
            icon="debug-restart"
            busy={busy[`retryGroup:${group.id}`]}
            onClick={() => send({ type: 'retryGroup', groupId: group.id })}
          >
            Retry {failed.length} failed shard{failed.length === 1 ? '' : 's'}
          </Button>
        ) : null}
        <Button
          kind="ghost"
          icon="copy"
          title="Copy group id"
          onClick={() => send({ type: 'copy', text: group.id })}
        />
      </div>
      <div className="grid-2">
        <Card
          title={
            <>
              <Icon name="graph" /> Timeline
            </>
          }
        >
          <Timeline group={group} now={now} />
        </Card>
        <Card
          title={
            <>
              <Icon name="beaker" /> Tests
            </>
          }
        >
          {active ? (
            <p className="muted small">Results appear when every shard has finished.</p>
          ) : (
            <Tests summary={tests} />
          )}
        </Card>
      </div>
      <Card
        title={
          <>
            <Icon name="layers" /> Shards
          </>
        }
      >
        <table className="table">
          <thead>
            <tr>
              <th style={{ width: 28 }} aria-label="State" />
              <th style={{ width: 70 }}>Shard</th>
              <th>Job</th>
              <th style={{ width: 120 }}>Node</th>
              <th style={{ width: 90 }}>Duration</th>
              <th style={{ width: 80 }}>Attempt</th>
            </tr>
          </thead>
          <tbody>
            {group.jobs.map((j) => (
              <tr
                key={j.id}
                className="clickable"
                onClick={() => dispatch({ type: 'go', view: 'job', jobId: j.id })}
              >
                <td>
                  <StateIcon state={j.state} />
                </td>
                <td>
                  {j.shardIndex}/{j.shardTotal}
                </td>
                <td className="ellipsis">
                  <span className="mono small">{j.id}</span>
                  {j.error && j.state !== 'succeeded' ? (
                    <div className="error-text small ellipsis">{j.error}</div>
                  ) : null}
                </td>
                <td>{j.nodeName ?? '–'}</td>
                <td>
                  {j.startedAt
                    ? formatDuration((j.finishedAt ?? now) - j.startedAt)
                    : isActive(j.state)
                      ? '…'
                      : '–'}
                </td>
                <td>{j.attempt}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>
    </div>
  );
}

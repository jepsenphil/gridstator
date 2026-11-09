import { useEffect, useState, type Dispatch } from 'react';
import type { Job, JobEvent, JobStep } from '@gridstator/protocol';
import type { DashboardState } from '../../src/messages';
import type { Action, JobLogs } from '../store';
import { ago, formatBytes, formatDuration, send } from '../util';
import {
  Button,
  Card,
  Empty,
  Icon,
  KindIcon,
  StateBadge,
  isActive,
  isTerminalState,
  kindLabel,
} from './common';
import { LogViewer } from './LogViewer';

function specSummary(job: Job): Array<[string, string]> {
  const s = job.spec;
  switch (s.kind) {
    case 'command':
      return [
        ['Image', s.image],
        [
          'Command',
          s.command.length >= 3 && s.command[0] === 'sh' && s.command[1] === '-c'
            ? s.command[2]!
            : s.command.join(' '),
        ],
        ['Working dir', s.workdir],
      ];
    case 'docker-build':
      return [
        [
          'Dockerfile',
          s.dockerfile ?? (s.context === '.' ? 'Dockerfile' : `${s.context}/Dockerfile`),
        ],
        ['Context', s.context],
        ['Target stage', s.stage ?? '(final)'],
        ['Output', `${s.output}${s.tags.length ? ` → ${s.tags.join(', ')}` : ''}`],
      ];
    case 'actions':
      return [
        ['Workflow', s.workflow],
        ['Workflow job', s.job ?? 'whole workflow'],
        ['Event', s.event],
        ...(Object.keys(s.inputs).length
          ? ([
              [
                'Inputs',
                Object.entries(s.inputs)
                  .map(([k, v]) => `${k}=${v}`)
                  .join(', '),
              ],
            ] as Array<[string, string]>)
          : []),
      ];
  }
}

function Steps({ steps }: { steps: JobStep[] }) {
  const groups = new Map<string, JobStep[]>();
  for (const s of steps) {
    const g = s.group ?? '';
    groups.set(g, [...(groups.get(g) ?? []), s]);
  }
  const icon = (s: JobStep) =>
    s.status === 'success' ? (
      <Icon name="pass-filled" className="state-succeeded" />
    ) : s.status === 'failure' ? (
      <Icon name="error" className="state-failed" />
    ) : s.status === 'running' ? (
      <Icon name="sync" spin className="state-running" />
    ) : s.status === 'cancelled' ? (
      <Icon name="circle-slash" className="state-cancelled" />
    ) : (
      <Icon name="circle-outline" className="muted" />
    );
  return (
    <div className="steps">
      {[...groups].map(([group, list]) => (
        <div key={group} className="step-group">
          {group ? (
            <div className="step-group-title">
              <Icon name="github-action" /> {group}
            </div>
          ) : null}
          <ol>
            {list.map((s) => (
              <li key={s.id} className={`step step-${s.status}`}>
                {icon(s)} <span className="ellipsis">{s.name}</span>
                <span className="muted small">
                  {s.durationMs !== null
                    ? formatDuration(s.durationMs)
                    : s.status === 'running'
                      ? 'running'
                      : ''}
                </span>
              </li>
            ))}
          </ol>
        </div>
      ))}
    </div>
  );
}

function History({ events }: { events: JobEvent[] }) {
  return (
    <ol className="history">
      {events.map((e) => (
        <li key={e.id}>
          <span className="muted small mono">{new Date(e.ts).toLocaleTimeString()}</span>{' '}
          {e.toState ? (
            <span>
              {e.fromState ?? '·'} → <strong>{e.toState}</strong>
            </span>
          ) : (
            <strong>{e.type}</strong>
          )}{' '}
          <span className="muted">
            {e.actor}
            {e.message ? ` — ${e.message}` : ''}
          </span>
        </li>
      ))}
    </ol>
  );
}

export function JobDetail({
  data,
  jobId,
  logs,
  events,
  busy,
  dispatch,
}: {
  data: DashboardState;
  jobId: string | null;
  logs: JobLogs | undefined;
  events: JobEvent[] | undefined;
  busy: Record<string, boolean>;
  dispatch: Dispatch<Action>;
}) {
  const job = data.jobs.find((j) => j.id === jobId);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    send({ type: 'selectJob', jobId });
    return () => send({ type: 'selectJob', jobId: null });
  }, [jobId]);
  useEffect(() => {
    if (!job || !isActive(job.state)) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [job]);
  if (!jobId || !job) {
    return (
      <Empty icon="search" title="Job not found">
        It may be older than the loaded history.{' '}
        <button className="link" onClick={() => dispatch({ type: 'go', view: 'jobs' })}>
          Back to jobs
        </button>
      </Empty>
    );
  }
  const terminal = isTerminalState(job.state);
  const mine = data.myJobIds.includes(job.id);
  const duration = job.startedAt ? formatDuration((job.finishedAt ?? now) - job.startedAt) : '–';
  const retryOf = job.retryOf;
  return (
    <div className="stack job-detail">
      <div className="job-header">
        <Button
          kind="ghost"
          icon="arrow-left"
          title="Back to jobs"
          onClick={() => dispatch({ type: 'go', view: 'jobs' })}
        />
        <KindIcon kind={job.kind} />
        <h2 className="ellipsis" title={job.name}>
          {job.name}
        </h2>
        <StateBadge state={job.state} />
        <span className="spacer" />
        {!terminal ? (
          <Button
            kind="danger"
            icon="debug-stop"
            busy={busy[`cancel:${job.id}`]}
            disabled={job.cancelRequestedAt !== null}
            onClick={() => send({ type: 'cancel', jobId: job.id })}
          >
            {job.cancelRequestedAt ? 'Cancelling…' : 'Cancel'}
          </Button>
        ) : (
          <>
            <Button
              kind="primary"
              icon="debug-restart"
              busy={busy[`retry:${job.id}`]}
              title="Run again with the same snapshot and settings"
              onClick={() => send({ type: 'retry', jobId: job.id })}
            >
              Retry
            </Button>
            <Button
              kind="secondary"
              icon="refresh"
              busy={busy[`rerun:${job.id}`]}
              title="Run again with the current files in your workspace"
              onClick={() => send({ type: 'rerun', jobId: job.id })}
            >
              Rerun with current files
            </Button>
          </>
        )}
        <Button
          kind="ghost"
          icon="copy"
          title="Copy job id"
          onClick={() => send({ type: 'copy', text: job.id })}
        />
      </div>

      {job.error && job.state !== 'succeeded' ? (
        <div className="banner banner-error">
          <Icon name="error" /> {job.error}
          {job.exitCode !== null ? (
            <span className="muted"> (exit code {job.exitCode})</span>
          ) : null}
        </div>
      ) : null}
      {job.state === 'queued' && job.stateReason ? (
        <div className="banner banner-info">
          <Icon name="clock" /> Queued #{job.queuePosition}: {job.stateReason}
        </div>
      ) : null}

      <div className="detail-grid">
        <Card title="Details">
          <dl className="props">
            <dt>Job</dt>
            <dd className="mono">{job.id}</dd>
            <dt>Kind</dt>
            <dd>{kindLabel(job)}</dd>
            <dt>Node</dt>
            <dd>{job.nodeName ?? '–'}</dd>
            <dt>Started by</dt>
            <dd>
              {job.owner}
              {mine ? <span className="pill">this window</span> : null}
            </dd>
            <dt>Attempt</dt>
            <dd>
              {job.attempt}
              {retryOf ? (
                <>
                  {' '}
                  (retry of{' '}
                  <button
                    className="link mono"
                    onClick={() => dispatch({ type: 'go', view: 'job', jobId: retryOf })}
                  >
                    {retryOf.slice(-8)}
                  </button>
                  )
                </>
              ) : null}
            </dd>
            <dt>Snapshot</dt>
            <dd className="mono" title={job.snapshotId ?? ''}>
              {job.snapshotId ? job.snapshotId.slice(7, 19) : 'none'}
            </dd>
            <dt>Resources</dt>
            <dd>
              {job.spec.resources.cpus} CPU ·{' '}
              {formatBytes(job.spec.resources.memoryMb * 1024 * 1024)}
            </dd>
            <dt>Created</dt>
            <dd title={new Date(job.createdAt).toLocaleString()}>{ago(job.createdAt, now)}</dd>
            <dt>Duration</dt>
            <dd>{duration}</dd>
            {specSummary(job).map(([k, v]) => (
              <span key={k} className="contents">
                <dt>{k}</dt>
                <dd className="mono ellipsis" title={v}>
                  {v}
                </dd>
              </span>
            ))}
          </dl>
        </Card>

        <div className="stack">
          {job.outputs?.image ? (
            <Card
              title={
                <>
                  <Icon name="package" /> Image
                </>
              }
            >
              <dl className="props">
                <dt>ID</dt>
                <dd className="mono ellipsis" title={job.outputs.image.id ?? ''}>
                  {job.outputs.image.id ?? '–'}
                </dd>
                {job.outputs.image.digest ? (
                  <>
                    <dt>Digest</dt>
                    <dd className="mono ellipsis" title={job.outputs.image.digest}>
                      {job.outputs.image.digest}
                    </dd>
                  </>
                ) : null}
                <dt>Tags</dt>
                <dd>{job.outputs.image.tags.join(', ') || 'untagged'}</dd>
                <dt>Size</dt>
                <dd>{formatBytes(job.outputs.image.sizeBytes)}</dd>
                <dt>Output</dt>
                <dd>
                  {job.outputs.image.output === 'load'
                    ? `loaded on ${job.nodeName}`
                    : job.outputs.image.output}
                </dd>
              </dl>
            </Card>
          ) : null}
          {job.outputs?.steps?.length ? (
            <Card
              title={
                <>
                  <Icon name="list-ordered" /> Steps
                </>
              }
            >
              <Steps steps={job.outputs.steps} />
            </Card>
          ) : null}
          <Card
            title={
              <>
                <Icon name="archive" /> Artifacts
              </>
            }
          >
            {job.artifacts.length === 0 ? (
              <p className="muted small">
                {job.kind === 'command'
                  ? 'Collect files with "artifacts" or "junit" in a gridstator.yml task.'
                  : 'None.'}
              </p>
            ) : (
              <ul className="artifacts">
                {job.artifacts.map((a) => (
                  <li key={a.name}>
                    <Icon name="file-binary" />{' '}
                    <span className="ellipsis" title={a.name}>
                      {a.name}
                    </span>
                    <span className="muted small">{formatBytes(a.size)}</span>
                    <Button
                      kind="ghost"
                      icon="cloud-download"
                      title={`Download ${a.name}`}
                      busy={busy[`artifact:${job.id}:${a.name}`]}
                      onClick={() =>
                        send({ type: 'downloadArtifact', jobId: job.id, name: a.name })
                      }
                    />
                  </li>
                ))}
              </ul>
            )}
          </Card>
          <Card
            title={
              <>
                <Icon name="history" /> History
              </>
            }
          >
            {events ? <History events={events} /> : <p className="muted small">Loading…</p>}
          </Card>
        </div>
      </div>

      <Card
        title={
          <>
            <Icon name="output" /> Logs
          </>
        }
        className="logs-card"
      >
        <LogViewer jobId={job.id} logs={logs} active={!terminal} />
      </Card>
    </div>
  );
}

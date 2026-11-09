import { useMemo, useState, type Dispatch } from 'react';
import type { Job } from '@gridstator/protocol';
import type { DashboardState } from '../../src/messages';
import type { Action } from '../store';
import { ago, cx, formatDuration, send } from '../util';
import {
  Button,
  Card,
  Empty,
  Icon,
  KindIcon,
  StateIcon,
  isActive,
  isTerminalState,
  kindLabel,
} from './common';
import { GroupStateIcon } from './GroupDetail';

type Filter = 'all' | 'active' | 'queued' | 'succeeded' | 'failed' | 'stopped';
const FILTERS: Array<{ id: Filter; label: string; match: (j: Job) => boolean }> = [
  { id: 'all', label: 'All', match: () => true },
  { id: 'active', label: 'Running', match: (j) => isActive(j.state) },
  { id: 'queued', label: 'Queued', match: (j) => j.state === 'queued' || j.state === 'created' },
  { id: 'succeeded', label: 'Succeeded', match: (j) => j.state === 'succeeded' },
  { id: 'failed', label: 'Failed', match: (j) => j.state === 'failed' || j.state === 'lost' },
  { id: 'stopped', label: 'Cancelled', match: (j) => j.state === 'cancelled' },
];

export function Jobs({
  data,
  dispatch,
  busy,
}: {
  data: DashboardState;
  dispatch: Dispatch<Action>;
  busy: Record<string, boolean>;
}) {
  const [filter, setFilter] = useState<Filter>('all');
  const [kind, setKind] = useState('');
  const [mine, setMine] = useState(false);
  const [query, setQuery] = useState('');
  const myJobs = useMemo(() => new Set(data.myJobIds), [data.myJobIds]);
  const now = Date.now();
  const rows = data.jobs.filter((j) => {
    if (!FILTERS.find((f) => f.id === filter)!.match(j)) return false;
    if (kind && j.kind !== kind) return false;
    if (mine && !myJobs.has(j.id)) return false;
    if (query) {
      const q = query.toLowerCase();
      if (![j.name, j.id, j.nodeName ?? '', j.owner].some((s) => s.toLowerCase().includes(q)))
        return false;
    }
    return true;
  });
  const groups = data.groups.slice(0, 8);
  return (
    <div className="stack">
      {groups.length ? (
        <Card
          title={
            <>
              <Icon name="layers" /> Sharded runs
            </>
          }
        >
          <table className="table">
            <tbody>
              {groups.map((g) => (
                <tr
                  key={g.id}
                  className="clickable"
                  onClick={() => dispatch({ type: 'go', view: 'group', groupId: g.id })}
                >
                  <td style={{ width: 28 }}>
                    <GroupStateIcon state={g.state} />
                  </td>
                  <td className="ellipsis">{g.name}</td>
                  <td style={{ width: 150 }}>
                    {g.counts.succeeded ?? 0}/{g.shardTotal} shards passed
                  </td>
                  <td style={{ width: 160 }} className="muted ellipsis">
                    {g.nodes.join(', ') || '–'}
                  </td>
                  <td style={{ width: 90 }}>
                    {g.startedAt ? formatDuration((g.finishedAt ?? now) - g.startedAt) : '–'}
                  </td>
                  <td style={{ width: 90 }}>{ago(g.createdAt, now)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      ) : null}
      <div className="toolbar">
        <div className="chips" role="tablist">
          {FILTERS.map((f) => {
            const count = data.jobs.filter(f.match).length;
            return (
              <button
                key={f.id}
                role="tab"
                aria-selected={filter === f.id}
                className={cx('chip', filter === f.id && 'chip-on')}
                onClick={() => setFilter(f.id)}
              >
                {f.label} <span className="chip-count">{count}</span>
              </button>
            );
          })}
        </div>
        <div className="toolbar-right">
          <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Job kind">
            <option value="">All kinds</option>
            <option value="command">Commands</option>
            <option value="docker-build">Docker builds</option>
            <option value="actions">GitHub Actions</option>
          </select>
          <label className="check">
            <input type="checkbox" checked={mine} onChange={(e) => setMine(e.target.checked)} />{' '}
            Started here
          </label>
          <input
            className="search"
            type="search"
            placeholder="Search jobs"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
      </div>
      {rows.length === 0 ? (
        <Empty icon="search" title="No matching jobs" />
      ) : (
        <table className="table jobs-table">
          <thead>
            <tr>
              <th aria-label="State" />
              <th>Name</th>
              <th>Kind</th>
              <th>Node</th>
              <th>Duration</th>
              <th>Created</th>
              <th>Owner</th>
              <th aria-label="Actions" />
            </tr>
          </thead>
          <tbody>
            {rows.map((j) => (
              <tr
                key={j.id}
                className="clickable"
                onClick={() => dispatch({ type: 'go', view: 'job', jobId: j.id })}
              >
                <td>
                  <StateIcon state={j.state} />
                </td>
                <td className="ellipsis name-cell" title={j.name}>
                  {j.name}
                  {j.attempt > 1 ? <span className="pill">attempt {j.attempt}</span> : null}
                  {j.shardIndex !== null ? (
                    <span className="pill">
                      shard {j.shardIndex}/{j.shardTotal}
                    </span>
                  ) : null}
                  {j.state === 'queued' && j.stateReason ? (
                    <div className="muted small ellipsis">
                      #{j.queuePosition} · {j.stateReason}
                    </div>
                  ) : null}
                  {j.error && j.state !== 'succeeded' ? (
                    <div className="error-text small ellipsis">{j.error}</div>
                  ) : null}
                </td>
                <td>
                  <KindIcon kind={j.kind} /> {kindLabel(j)}
                </td>
                <td>{j.nodeName ?? '–'}</td>
                <td>{j.startedAt ? formatDuration((j.finishedAt ?? now) - j.startedAt) : '–'}</td>
                <td title={new Date(j.createdAt).toLocaleString()}>{ago(j.createdAt, now)}</td>
                <td className="muted">{j.owner}</td>
                <td className="row-actions" onClick={(e) => e.stopPropagation()}>
                  {isTerminalState(j.state) ? (
                    <Button
                      kind="ghost"
                      icon="debug-restart"
                      title="Retry (same snapshot)"
                      busy={busy[`retry:${j.id}`]}
                      onClick={() => send({ type: 'retry', jobId: j.id })}
                    />
                  ) : (
                    <Button
                      kind="ghost"
                      icon="debug-stop"
                      title="Cancel"
                      busy={busy[`cancel:${j.id}`]}
                      disabled={j.cancelRequestedAt !== null}
                      onClick={() => send({ type: 'cancel', jobId: j.id })}
                    />
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

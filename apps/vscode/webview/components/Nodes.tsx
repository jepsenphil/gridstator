import { useEffect, useRef, useState, type Dispatch } from 'react';
import type { Node, SchedulingMode } from '@gridstator/protocol';
import type { DashboardState } from '../../src/messages';
import type { Action } from '../store';
import { ago, cx, formatBytes, send } from '../util';
import { Button, Card, Empty, Icon, Meter, nodeStatus } from './common';

function NodeCard({
  node,
  data,
  busy,
  focused,
  dispatch,
}: {
  node: Node;
  data: DashboardState;
  busy: boolean;
  focused: boolean;
  dispatch: Dispatch<Action>;
}) {
  const [concurrency, setConcurrency] = useState(String(node.settings.maxConcurrency));
  const [cpus, setCpus] = useState(String(node.settings.reserved.cpus));
  const [mem, setMem] = useState(
    String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10),
  );
  const [labels, setLabels] = useState(node.settings.labels.join(', '));
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setConcurrency(String(node.settings.maxConcurrency));
    setCpus(String(node.settings.reserved.cpus));
    setMem(String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10));
    setLabels(node.settings.labels.join(', '));
  }, [node.settings]);
  useEffect(() => {
    if (focused) ref.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }, [focused]);
  const st = nodeStatus(node);
  const dirty =
    concurrency !== String(node.settings.maxConcurrency) ||
    cpus !== String(node.settings.reserved.cpus) ||
    mem !== String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10) ||
    labels !== node.settings.labels.join(', ');
  const valid =
    /^\d+$/.test(concurrency) &&
    +concurrency >= 1 &&
    /^\d+(\.\d+)?$/.test(cpus) &&
    /^\d+(\.\d+)?$/.test(mem);
  const setMode = (mode: SchedulingMode) =>
    send({ type: 'updateNode', nodeId: node.id, patch: { schedulingMode: mode } });
  const save = () =>
    send({
      type: 'updateNode',
      nodeId: node.id,
      patch: {
        maxConcurrency: Number(concurrency),
        reserved: { cpus: Number(cpus), memoryMb: Math.round(Number(mem) * 1024) },
        labels: labels
          .split(',')
          .map((l) => l.trim())
          .filter(Boolean),
      },
    });
  const m = node.metrics;
  const info = node.info;
  const active = data.jobs.filter((j) => node.activeJobs.includes(j.id));
  return (
    <div ref={ref}>
      <Card
        className={cx('node-card', focused && 'card-focused')}
        title={
          <>
            <span className={`dot dot-${st.className}`} /> {node.name}{' '}
            <span className="muted small">{st.label}</span>
          </>
        }
        actions={
          <div className="segmented" role="radiogroup" aria-label={`Scheduling for ${node.name}`}>
            {(['enabled', 'draining', 'disabled'] as const).map((mode) => (
              <button
                key={mode}
                role="radio"
                aria-checked={node.settings.schedulingMode === mode}
                className={cx('seg', node.settings.schedulingMode === mode && 'seg-on')}
                disabled={busy}
                onClick={() => setMode(mode)}
                title={
                  mode === 'enabled'
                    ? 'Accept new jobs'
                    : mode === 'draining'
                      ? 'Finish running jobs, accept no new ones'
                      : 'Accept no new jobs'
                }
              >
                {mode === 'enabled' ? 'Enabled' : mode === 'draining' ? 'Drain' : 'Disabled'}
              </button>
            ))}
          </div>
        }
      >
        {node.healthIssues.map((h) => (
          <div key={h} className="banner banner-warn">
            <Icon name="warning" /> {h}
          </div>
        ))}
        <div className="node-grid">
          <div>
            <div className="metric">
              <span>CPU</span>
              <Meter value={m?.cpuPercent ?? 0} max={100} label="CPU" />
              <span className="small">
                {m ? `${Math.round(m.cpuPercent)}% of ${info?.cpus ?? '?'}` : '–'}
              </span>
            </div>
            <div className="metric">
              <span>Memory</span>
              <Meter
                value={info && m ? info.memoryTotalMb - m.memoryAvailableMb : 0}
                max={info?.memoryTotalMb ?? 1}
                label="Memory"
              />
              <span className="small">
                {m && info
                  ? `${formatBytes(m.memoryAvailableMb * 1024 * 1024)} free of ${formatBytes(info.memoryTotalMb * 1024 * 1024)}`
                  : '–'}
              </span>
            </div>
            <div className="metric">
              <span>Disk</span>
              <Meter
                value={m ? m.diskTotalMb - m.diskFreeMb : 0}
                max={m?.diskTotalMb ?? 1}
                label="Disk"
              />
              <span className="small">
                {m ? `${formatBytes(m.diskFreeMb * 1024 * 1024)} free` : '–'}
              </span>
            </div>
            <div className="metric">
              <span>Allocated</span>
              <Meter
                value={node.allocated.cpus}
                max={node.allocatable.cpus || 1}
                label="Allocated CPU"
                warnAt={0.9}
                danger={1}
              />
              <span className="small">
                {node.allocated.cpus}/{node.allocatable.cpus} CPU ·{' '}
                {formatBytes(node.allocated.memoryMb * 1024 * 1024)}/
                {formatBytes(node.allocatable.memoryMb * 1024 * 1024)}
              </span>
            </div>
            <dl className="props compact">
              <dt>System</dt>
              <dd>{info ? `${info.platform}/${info.arch} · ${info.hostname}` : '–'}</dd>
              <dt>Docker</dt>
              <dd>
                {info?.docker.available
                  ? info.docker.version
                  : `unavailable${info?.docker.error ? ` (${info.docker.error})` : ''}`}
              </dd>
              <dt>Runs</dt>
              <dd>
                {info?.executors.length ? info.executors.join(', ') : 'nothing (needs Docker)'}
              </dd>
              <dt>act</dt>
              <dd>
                {info?.act.available ? info.act.version : (info?.act.error ?? 'not installed')}
              </dd>
              <dt>Labels</dt>
              <dd>
                {node.effectiveLabels.map((l) => (
                  <span key={l} className="pill">
                    {l}
                  </span>
                ))}
              </dd>
              <dt>Last seen</dt>
              <dd>{ago(node.lastSeenAt)}</dd>
            </dl>
          </div>
          <form
            className="form compact-form"
            onSubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            <label className="field">
              <span className="label">Max concurrent jobs</span>
              <input
                type="number"
                min="1"
                max="256"
                value={concurrency}
                onChange={(e) => setConcurrency(e.target.value)}
              />
            </label>
            <div className="row">
              <label className="field grow">
                <span className="label">Reserved CPUs</span>
                <input
                  type="number"
                  min="0"
                  step="0.5"
                  value={cpus}
                  onChange={(e) => setCpus(e.target.value)}
                />
              </label>
              <label className="field grow">
                <span className="label">Reserved memory (GiB)</span>
                <input
                  type="number"
                  min="0"
                  step="0.5"
                  value={mem}
                  onChange={(e) => setMem(e.target.value)}
                />
              </label>
            </div>
            <span className="hint">
              Reserved resources are kept for the machine itself (for example your editor) and never
              given to jobs.
            </span>
            <label className="field">
              <span className="label">Labels</span>
              <input
                value={labels}
                onChange={(e) => setLabels(e.target.value)}
                placeholder="fast, big, gpu"
              />
            </label>
            <div className="form-actions">
              <Button type="submit" icon="save" disabled={!dirty || !valid} busy={busy}>
                Save
              </Button>
            </div>
            <div className="field">
              <span className="label">Active jobs</span>
              {active.length === 0 ? (
                <span className="muted small">None</span>
              ) : (
                active.map((j) => (
                  <button
                    key={j.id}
                    type="button"
                    className="link ellipsis"
                    onClick={() => dispatch({ type: 'go', view: 'job', jobId: j.id })}
                  >
                    {j.name}
                  </button>
                ))
              )}
            </div>
          </form>
        </div>
      </Card>
    </div>
  );
}

export function Nodes({
  data,
  focus,
  busy,
  dispatch,
}: {
  data: DashboardState;
  focus: string | null;
  busy: Record<string, boolean>;
  dispatch: Dispatch<Action>;
}) {
  if (data.nodes.length === 0) {
    return (
      <Empty icon="server" title="No worker nodes yet">
        On the controller run <code>gridstator token create --join</code>, then run the printed{' '}
        <code>gridstator agent join …</code> command on each worker.
      </Empty>
    );
  }
  return (
    <div className="stack">
      {data.nodes.map((n) => (
        <NodeCard
          key={n.id}
          node={n}
          data={data}
          busy={!!busy[`node:${n.id}`]}
          focused={focus === n.id}
          dispatch={dispatch}
        />
      ))}
    </div>
  );
}

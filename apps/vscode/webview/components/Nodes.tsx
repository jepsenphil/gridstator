import { useEffect, useRef, useState, type Dispatch } from 'react';
import type { Node, NodeAutoUpdate, SchedulingMode } from '@gridstator/protocol';
import { NodeNameSchema } from '@gridstator/protocol/node';
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
  const [name, setName] = useState(node.name);
  const [concurrency, setConcurrency] = useState(String(node.settings.maxConcurrency));
  const [cpus, setCpus] = useState(String(node.settings.reserved.cpus));
  const [mem, setMem] = useState(
    String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10),
  );
  const [labels, setLabels] = useState(node.settings.labels.join(', '));
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setName(node.name);
  }, [node.name]);
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
  const settingsDirty =
    concurrency !== String(node.settings.maxConcurrency) ||
    cpus !== String(node.settings.reserved.cpus) ||
    mem !== String(Math.round((node.settings.reserved.memoryMb / 1024) * 10) / 10) ||
    labels !== node.settings.labels.join(', ');
  const nameDirty = name !== node.name;
  const nameError = !NodeNameSchema.safeParse(name).success
    ? 'Use 1–64 letters, numbers, dots, underscores or hyphens; start with a letter or number.'
    : data.nodes.some((other) => other.id !== node.id && (other.name === name || other.id === name))
      ? 'Another worker already uses this name.'
      : null;
  const dirty = nameDirty || settingsDirty;
  const valid =
    (!nameDirty || !nameError) &&
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
        ...(nameDirty ? { name } : {}),
        ...(settingsDirty
          ? {
              maxConcurrency: Number(concurrency),
              reserved: { cpus: Number(cpus), memoryMb: Math.round(Number(mem) * 1024) },
              labels: labels
                .split(',')
                .map((l) => l.trim())
                .filter(Boolean),
            }
          : {}),
      },
    });
  const m = node.metrics;
  const info = node.info;
  const active = data.jobs.filter((j) => node.activeJobs.includes(j.id));
  const isController =
    !!data.connection.controller?.machineId &&
    node.info?.machineId === data.connection.controller.machineId;
  return (
    <div ref={ref}>
      <Card
        className={cx('node-card', focused && 'card-focused')}
        title={
          <>
            <span className={`dot dot-${st.className}`} /> {node.name}{' '}
            <span className="muted small">{st.label}</span>
            {isController ? <span className="pill">Controller + worker</span> : null}
            {node.update && node.update.state !== 'current' ? (
              <span
                className={cx('pill', 'update-pill', `update-${node.update.state}`)}
                title={node.update.detail ?? undefined}
              >
                {node.update.state === 'unknown' ? 'version unknown' : node.update.state}
              </span>
            ) : null}
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
        {node.update &&
        (node.update.state === 'behind' ||
          node.update.state === 'failed' ||
          node.update.state === 'updating') ? (
          <div
            className={cx(
              'banner',
              node.update.state === 'failed'
                ? 'banner-error'
                : node.update.state === 'updating'
                  ? 'banner-info'
                  : 'banner-warn',
            )}
          >
            <Icon name={node.update.state === 'updating' ? 'sync' : 'cloud-download'} />{' '}
            {node.update.state === 'updating'
              ? 'Updating'
              : node.update.state === 'failed'
                ? 'Update failed'
                : 'Not on the controller’s build'}
            {node.update.detail ? `: ${node.update.detail}` : ''}
          </div>
        ) : null}
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
              <dt>Build</dt>
              <dd>
                {node.update?.build ??
                  info?.build?.id ??
                  (info ? `v${info.agentVersion} (no build id)` : '–')}
                {info?.build?.runtime === 'rust'
                  ? ` · Rust${info.build.target ? ` · ${info.build.target}` : ''}`
                  : info?.build?.node
                    ? ` · node ${info.build.node}`
                    : ''}
              </dd>
              <dt>Auto-update</dt>
              <dd>
                <select
                  aria-label={`Auto-update for ${node.name}`}
                  value={node.settings.autoUpdate}
                  disabled={busy}
                  onChange={(e) =>
                    send({
                      type: 'updateNode',
                      nodeId: node.id,
                      patch: { autoUpdate: e.target.value as NodeAutoUpdate },
                    })
                  }
                >
                  <option value="inherit">
                    Cluster default{node.update ? ` (${node.update.autoUpdate})` : ''}
                  </option>
                  <option value="follow">Follow the controller</option>
                  <option value="window">In the update window</option>
                  <option value="off">Off</option>
                </select>
              </dd>
              <dt>Last seen</dt>
              <dd>{ago(node.lastSeenAt)}</dd>
            </dl>
          </div>
          <form
            className="form compact-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (dirty && valid && !busy) save();
            }}
          >
            <label className="field">
              <span className="label">Worker name</span>
              <input
                value={name}
                maxLength={64}
                disabled={busy}
                onChange={(event) => setName(event.target.value)}
                aria-invalid={nameDirty && !!nameError}
                aria-describedby={`worker-name-hint-${node.id}`}
                autoComplete="off"
                spellCheck={false}
              />
              <span
                id={`worker-name-hint-${node.id}`}
                className={nameDirty && nameError ? 'error-text small' : 'hint'}
              >
                {nameDirty && nameError
                  ? nameError
                  : 'Use this name when choosing a worker for a job. History and settings are kept.'}
              </span>
            </label>
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
  const controller = data.connection.controller;
  const controllerHasWorker =
    !!controller && data.nodes.some((node) => node.info?.machineId === controller.machineId);
  const notCurrent = data.nodes.filter(
    (n) => n.update && n.update.state !== 'current' && n.update.state !== 'unknown',
  );
  const build = data.nodes.find((n) => n.update?.state === 'current')?.update?.build;
  return (
    <div className="stack">
      <div className="form-actions">
        <span className="spacer" />
        <Button
          icon="add"
          disabled={data.connection.status !== 'connected' || data.connection.role !== 'admin'}
          title={
            data.connection.role === 'admin'
              ? 'Generate a worker join command'
              : 'Admin access is required to add workers'
          }
          onClick={() => dispatch({ type: 'go', view: 'addWorker' })}
        >
          Add worker
        </Button>
      </div>
      {controller && !controllerHasWorker ? (
        <Card
          className="controller-card"
          title={
            <>
              <Icon name="server-environment" /> {controller.hostname}
              <span className="pill">Controller</span>
            </>
          }
          actions={
            <Button
              kind="secondary"
              icon="settings-gear"
              onClick={() => dispatch({ type: 'go', view: 'settings' })}
            >
              Settings
            </Button>
          }
        >
          <dl className="props compact">
            <dt>Connection</dt>
            <dd>
              {data.connection.status} · {data.connection.url}
            </dd>
            <dt>Version</dt>
            <dd>{data.connection.version ?? 'Unknown'}</dd>
            <dt>Build</dt>
            <dd>
              <code>{data.connection.build?.id ?? 'Not reported'}</code>
            </dd>
            <dt>Worker</dt>
            <dd>This machine is not registered as a worker.</dd>
          </dl>
        </Card>
      ) : null}
      {notCurrent.length > 0 ? (
        <div className="banner banner-warn">
          <Icon name="cloud-download" /> {notCurrent.length} reachable worker(s) not on the
          controller’s build: {notCurrent.map((n) => `${n.name} (${n.update?.state})`).join(', ')}.
          <Button kind="ghost" onClick={() => dispatch({ type: 'go', view: 'settings' })}>
            Manage updates
          </Button>
        </div>
      ) : build ? (
        <div className="muted small">Every reachable worker runs {build}.</div>
      ) : null}
      {data.nodes.length === 0 ? (
        <Empty icon="server" title="No worker nodes yet">
          Choose Add worker to generate a join command for another computer.
        </Empty>
      ) : null}
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

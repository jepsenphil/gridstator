import { useEffect, useMemo, useState } from 'react';
import type { DashboardState, LaunchItem, LaunchKind, LaunchOptions } from '../../src/messages';
import { cx, parseKeyValues, send } from '../util';
import { Button, Card, Empty, Icon } from './common';

const SECTIONS: Array<{ kind: LaunchKind; label: string; icon: string }> = [
  { kind: 'task', label: 'Tasks', icon: 'checklist' },
  { kind: 'build', label: 'Builds', icon: 'package' },
  { kind: 'workflow', label: 'GitHub Actions', icon: 'github-action' },
  { kind: 'script', label: 'Package scripts', icon: 'symbol-event' },
  { kind: 'dockerfile', label: 'Dockerfiles', icon: 'file-code' },
];

function commandItem(folder: string): LaunchItem {
  return {
    id: `command:${folder}`,
    kind: 'command',
    label: 'Custom command',
    folder,
    warnings: [],
  };
}

function Form({ item, data, busy }: { item: LaunchItem; data: DashboardState; busy: boolean }) {
  const [node, setNode] = useState(data.launcher.defaultNode);
  const [preferOnly, setPreferOnly] = useState(false);
  const [shards, setShards] = useState(String(item.task?.shards ?? 1));
  const shardable = item.kind === 'task' || item.kind === 'script' || item.kind === 'command';
  const [cpus, setCpus] = useState('');
  const [memory, setMemory] = useState('');
  const [env, setEnv] = useState('');
  const [job, setJob] = useState('');
  const [event, setEvent] = useState(item.workflow?.defaultEvent ?? 'push');
  const [inputs, setInputs] = useState<Record<string, string>>(() =>
    Object.fromEntries(
      (item.workflow?.inputs ?? []).map((i) => [i.name, i.default ?? i.options?.[0] ?? '']),
    ),
  );
  const [stage, setStage] = useState(item.build?.stage ?? '');
  const [tags, setTags] = useState(item.build?.tags.join(', ') ?? '');
  const [output, setOutput] = useState<LaunchOptions['output']>(
    (item.build?.output as LaunchOptions['output']) ?? 'load',
  );
  const [buildArgs, setBuildArgs] = useState('');
  const [command, setCommand] = useState('');
  const [image, setImage] = useState('');

  useEffect(() => {
    const [, preJob] = item.id.split('#');
    if (preJob) setJob(preJob);
  }, [item.id]);

  const stages = item.dockerfile?.stages ?? [];
  const jobs = item.workflow?.jobs ?? [];
  const selectedJob = jobs.find((j) => j.id === job);
  const runnable = !selectedJob || selectedJob.runnable;

  const launch = () => {
    const options: LaunchOptions = {
      ...(preferOnly && node
        ? { node: '', prefer: [node] }
        : node !== data.launcher.defaultNode || node
          ? { node }
          : {}),
      ...(shardable && Number(shards) !== (item.task?.shards ?? 1)
        ? { shards: Math.max(1, Math.min(64, Number(shards) || 1)) }
        : {}),
      ...(cpus ? { cpus: Number(cpus) } : {}),
      ...(memory ? { memoryGb: Number(memory) } : {}),
      ...(env.trim() ? { env: parseKeyValues(env) } : {}),
    };
    if (item.kind === 'workflow')
      Object.assign(options, {
        ...(job ? { job } : {}),
        event,
        inputs:
          event === 'workflow_dispatch'
            ? Object.fromEntries(Object.entries(inputs).filter(([, v]) => v !== ''))
            : {},
      });
    if (item.kind === 'dockerfile' || item.kind === 'build') {
      Object.assign(options, {
        ...(stage ? { stage } : {}),
        tags: tags
          .split(',')
          .map((t) => t.trim())
          .filter(Boolean),
        output,
        ...(buildArgs.trim() ? { buildArgs: parseKeyValues(buildArgs) } : {}),
      });
    }
    if (item.kind === 'command') Object.assign(options, { command, ...(image ? { image } : {}) });
    send({ type: 'launch', request: { itemId: item.id.split('#')[0]!, options } });
  };

  return (
    <form
      className="form"
      onSubmit={(e) => {
        e.preventDefault();
        launch();
      }}
    >
      <div className="form-title">
        <h3>{item.label}</h3>
        {item.description ? <p className="muted">{item.description}</p> : null}
      </div>
      {item.warnings.map((w) => (
        <div key={w} className="banner banner-warn">
          <Icon name="warning" /> {w}
        </div>
      ))}

      {item.kind === 'task' && item.task ? (
        <div className="field">
          <span className="label">Command</span>
          <code className="code-block">{item.task.command}</code>
          {item.task.junit ? (
            <span className="hint">
              JUnit results from {item.task.junit} appear in the Testing view.
            </span>
          ) : null}
        </div>
      ) : null}
      {item.kind === 'script' && item.script ? (
        <div className="field">
          <span className="label">Runs</span>
          <code className="code-block">{item.script.command}</code>
          <span className="hint">
            Dependencies install from the worker's persistent {item.script.packageManager} cache.
          </span>
        </div>
      ) : null}
      {item.kind === 'command' ? (
        <>
          <label className="field">
            <span className="label">Command</span>
            <input
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="npm ci && npm test"
              required
              autoFocus
            />
            <span className="hint">
              Runs with sh -c from your project root, against your current files.
            </span>
          </label>
          <label className="field">
            <span className="label">Image</span>
            <input
              value={image}
              onChange={(e) => setImage(e.target.value)}
              placeholder="default from gridstator.yml (node:24-bookworm)"
            />
          </label>
        </>
      ) : null}

      {item.kind === 'workflow' ? (
        <>
          <label className="field">
            <span className="label">Job</span>
            <select aria-label="Job" value={job} onChange={(e) => setJob(e.target.value)}>
              <option value="">Whole workflow ({jobs.length} jobs, dependencies in order)</option>
              {jobs.map((j) => (
                <option key={j.id} value={j.id} disabled={!j.runnable}>
                  {j.id}
                  {j.name !== j.id ? ` — ${j.name}` : ''}
                  {!j.runnable
                    ? ' (cannot run locally)'
                    : j.needs.length
                      ? ` (needs ${j.needs.join(', ')})`
                      : ''}
                </option>
              ))}
            </select>
            {selectedJob?.warnings.map((w) => (
              <span key={w} className="hint warn-text">
                <Icon name="warning" /> {w}
              </span>
            ))}
          </label>
          <label className="field">
            <span className="label">Event</span>
            <select aria-label="Event" value={event} onChange={(e) => setEvent(e.target.value)}>
              {(item.workflow?.events.length ? item.workflow.events : ['push']).map((ev) => (
                <option key={ev} value={ev}>
                  {ev}
                </option>
              ))}
            </select>
          </label>
          {event === 'workflow_dispatch'
            ? item.workflow?.inputs.map((i) => (
                <label key={i.name} className="field">
                  <span className="label">
                    {i.name}
                    {i.required ? ' *' : ''}
                  </span>
                  {i.options?.length ? (
                    <select
                      aria-label={i.name}
                      value={inputs[i.name] ?? ''}
                      onChange={(e) => setInputs({ ...inputs, [i.name]: e.target.value })}
                    >
                      {i.options.map((o) => (
                        <option key={o}>{o}</option>
                      ))}
                    </select>
                  ) : i.type === 'boolean' ? (
                    <select
                      aria-label={i.name}
                      value={inputs[i.name] ?? 'false'}
                      onChange={(e) => setInputs({ ...inputs, [i.name]: e.target.value })}
                    >
                      <option>true</option>
                      <option>false</option>
                    </select>
                  ) : (
                    <input
                      value={inputs[i.name] ?? ''}
                      required={i.required && !i.default}
                      onChange={(e) => setInputs({ ...inputs, [i.name]: e.target.value })}
                    />
                  )}
                  {i.description ? <span className="hint">{i.description}</span> : null}
                </label>
              ))
            : null}
        </>
      ) : null}

      {item.kind === 'dockerfile' || item.kind === 'build' ? (
        <>
          <label className="field">
            <span className="label">Target stage</span>
            {stages.length ? (
              <select
                aria-label="Target stage"
                value={stage}
                onChange={(e) => setStage(e.target.value)}
              >
                <option value="">Final stage</option>
                {stages.map((s) => (
                  <option key={s}>{s}</option>
                ))}
              </select>
            ) : (
              <input
                value={stage}
                onChange={(e) => setStage(e.target.value)}
                placeholder="final stage"
              />
            )}
          </label>
          <label className="field">
            <span className="label">Tags</span>
            <input
              value={tags}
              onChange={(e) => setTags(e.target.value)}
              placeholder="my-app:dev, my-app:latest"
            />
          </label>
          <label className="field">
            <span className="label">Output</span>
            <select
              aria-label="Output"
              value={output}
              onChange={(e) => setOutput(e.target.value as LaunchOptions['output'])}
            >
              <option value="load">Load into the worker's Docker</option>
              <option value="tar">Export as a downloadable artifact (image.tar)</option>
              <option value="push">Push to the registry named by the tags</option>
              <option value="none">Build only (check the Dockerfile, warm the cache)</option>
            </select>
          </label>
          <label className="field">
            <span className="label">Build arguments</span>
            <textarea
              rows={2}
              value={buildArgs}
              onChange={(e) => setBuildArgs(e.target.value)}
              placeholder="KEY=value (one per line)"
            />
          </label>
        </>
      ) : null}

      <fieldset className="fieldset">
        <legend>Where</legend>
        <div className="row">
          <label className="field grow">
            <span className="label">Worker</span>
            <select aria-label="Worker" value={node} onChange={(e) => setNode(e.target.value)}>
              <option value="">Automatic (best available)</option>
              {data.nodes.map((n) => (
                <option key={n.id} value={n.name} disabled={n.status !== 'online'}>
                  {n.name} — {n.status}
                  {n.info ? `, ${n.info.cpus} CPU` : ''}, {n.activeJobs.length}/
                  {n.settings.maxConcurrency} jobs
                </option>
              ))}
            </select>
          </label>
          <label className="field small-field">
            <span className="label">CPUs</span>
            <input
              type="number"
              min="0.5"
              step="0.5"
              value={cpus}
              onChange={(e) => setCpus(e.target.value)}
              placeholder="default"
            />
          </label>
          <label className="field small-field">
            <span className="label">Memory (GiB)</span>
            <input
              type="number"
              min="0.25"
              step="0.25"
              value={memory}
              onChange={(e) => setMemory(e.target.value)}
              placeholder="default"
            />
          </label>
        </div>
        {node ? (
          <label className="check small">
            <input
              type="checkbox"
              checked={preferOnly}
              onChange={(e) => setPreferOnly(e.target.checked)}
            />{' '}
            Prefer this worker, but allow others when it is busy
          </label>
        ) : null}
        {shardable ? (
          <label className="field small-field">
            <span className="label">Shards</span>
            <input
              aria-label="Shards"
              type="number"
              min="1"
              max="64"
              value={shards}
              onChange={(e) => setShards(e.target.value)}
            />
            <span className="hint">
              Parallel copies on different workers; the command gets {'{shard}'}/{'{total}'} and
              GRIDSTATOR_SHARD_*
            </span>
          </label>
        ) : null}
        {node !== data.launcher.defaultNode ? (
          <button
            type="button"
            className="link small"
            onClick={() => send({ type: 'setDefaultNode', node })}
          >
            Use “{node || 'Automatic'}” by default for this workspace
          </button>
        ) : null}
        {item.kind !== 'dockerfile' && item.kind !== 'build' ? (
          <label className="field">
            <span className="label">Environment</span>
            <textarea
              rows={2}
              value={env}
              onChange={(e) => setEnv(e.target.value)}
              placeholder="KEY=value (one per line; not secret)"
            />
          </label>
        ) : null}
      </fieldset>

      <div className="form-actions">
        <Button
          type="submit"
          icon="play"
          busy={busy}
          disabled={!runnable || data.connection.status !== 'connected'}
        >
          {item.kind === 'workflow'
            ? job
              ? `Run job ${job}`
              : 'Run workflow'
            : item.kind === 'dockerfile' || item.kind === 'build'
              ? 'Build'
              : 'Run'}
        </Button>
        <span className="hint">
          Your saved files are snapshotted and sent to the worker. Unsaved editors are checked
          first.
        </span>
      </div>
    </form>
  );
}

export function Run({
  data,
  selected,
  busy,
  onSelect,
}: {
  data: DashboardState;
  selected: string | null;
  busy: Record<string, boolean>;
  onSelect: (id: string) => void;
}) {
  const [query, setQuery] = useState('');
  const folders = useMemo(
    () => [...new Set(data.launcher.items.map((i) => i.folder))],
    [data.launcher.items],
  );
  const items = data.launcher.items.filter((i) => !i.id.startsWith('error:'));
  const errors = data.launcher.items.filter((i) => i.id.startsWith('error:'));
  const q = query.toLowerCase();
  const visible = items.filter(
    (i) =>
      !q || i.label.toLowerCase().includes(q) || (i.description ?? '').toLowerCase().includes(q),
  );
  const baseId = selected?.split('#')[0];
  const current = selected?.startsWith('command:')
    ? commandItem(selected.slice('command:'.length))
    : items.find((i) => i.id === baseId);
  const currentItem =
    current && selected && current.id !== selected ? { ...current, id: selected } : current;
  const multi = folders.length > 1;
  return (
    <div className="run-layout">
      <aside className="run-list">
        <div className="search-wrap">
          <Icon name="search" />
          <input
            type="search"
            placeholder="Filter"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Filter runnable items"
          />
        </div>
        {errors.map((e) => (
          <div key={e.id} className="banner banner-error small">
            <Icon name="error" /> {e.warnings[0]}
          </div>
        ))}
        {(folders.length ? folders : ['']).map((folder) => (
          <button
            key={`cmd-${folder}`}
            className={cx('run-item', selected === `command:${folder}` && 'run-item-on')}
            onClick={() => onSelect(`command:${folder}`)}
          >
            <Icon name="terminal" /> <span>Custom command{multi ? ` · ${folder}` : ''}</span>
          </button>
        ))}
        {SECTIONS.map((s) => {
          const list = visible.filter((i) => i.kind === s.kind);
          if (list.length === 0) return null;
          return (
            <div key={s.kind} className="run-section">
              <div className="run-section-title">
                <Icon name={s.icon} /> {s.label}
              </div>
              {list.map((i) => (
                <button
                  key={i.id}
                  className={cx('run-item', baseId === i.id && 'run-item-on')}
                  onClick={() => onSelect(i.id)}
                  title={i.description}
                >
                  <span className="ellipsis">{i.label}</span>
                  {i.warnings.length ? (
                    <Icon name="warning" className="warn-text" title={i.warnings.join('\n')} />
                  ) : null}
                  {multi ? <span className="muted small">{i.folder}</span> : null}
                </button>
              ))}
            </div>
          );
        })}
        {data.launcher.loading ? (
          <div className="muted small">
            <Icon name="loading" spin /> Scanning workspace…
          </div>
        ) : null}
      </aside>
      <section className="run-form">
        {currentItem ? (
          <Card>
            <Form
              key={currentItem.id}
              item={currentItem}
              data={data}
              busy={!!busy[`launch:${currentItem.id.split('#')[0]}`]}
            />
          </Card>
        ) : (
          <Empty icon="play-circle" title="Choose something to run">
            Tasks and builds come from gridstator.yml; workflows from .github/workflows; scripts
            from package.json; and Dockerfiles from your project.
          </Empty>
        )}
      </section>
    </div>
  );
}

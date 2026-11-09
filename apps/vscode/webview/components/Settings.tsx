import { useEffect, useState, type ReactNode } from 'react';
import type { ClusterSettings, CreateTokenResponse } from '@gridstator/protocol';
import type { DashboardState, EditorPreferences, SettingsView } from '../../src/messages';
import { ago, send } from '../util';
import { Button, Empty } from './common';

type Busy = Record<string, boolean>;

function Section({
  id,
  title,
  description,
  children,
}: {
  id: string;
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <section id={`settings-${id}`} className="settings-section" aria-labelledby={`heading-${id}`}>
      <header className="settings-section-heading">
        <h2 id={`heading-${id}`}>{title}</h2>
        <p className="muted">{description}</p>
      </header>
      {children}
    </section>
  );
}

function Preferences({ settings, busy }: { settings: SettingsView; busy: Busy }) {
  const [draft, setDraft] = useState(settings.preferences);
  const saved = JSON.stringify(settings.preferences);
  useEffect(() => setDraft(JSON.parse(saved) as EditorPreferences), [saved]);
  const change = <K extends keyof EditorPreferences>(key: K, value: EditorPreferences[K]) =>
    setDraft((previous) => ({ ...previous, [key]: value }));
  return (
    <form
      className="form settings-form"
      onSubmit={(event) => {
        event.preventDefault();
        send({ type: 'savePreferences', patch: draft });
      }}
    >
      <label className="settings-check">
        <input
          type="checkbox"
          checked={draft.autoConnect}
          onChange={(event) => change('autoConnect', event.target.checked)}
        />
        Connect automatically using this machine’s shared login
      </label>
      <div className="settings-fields">
        <label className="field">
          <span className="label">Client configuration path</span>
          <input
            value={draft.configPath}
            placeholder="Use the CLI default"
            onChange={(event) => change('configPath', event.target.value)}
          />
          <span className="hint">Leave empty to share the CLI’s default login.</span>
        </label>
        <label className="field">
          <span className="label">Default worker</span>
          <input
            value={draft.defaultNode}
            placeholder="Automatic"
            onChange={(event) => change('defaultNode', event.target.value)}
          />
          <span className="hint">A worker name or ID; leave empty for automatic placement.</span>
        </label>
        <label className="field">
          <span className="label">Unsaved files</span>
          <select
            value={draft.unsavedFiles}
            onChange={(event) =>
              change('unsavedFiles', event.target.value as EditorPreferences['unsavedFiles'])
            }
          >
            <option value="prompt">Ask before running</option>
            <option value="saveAll">Save all files before running</option>
            <option value="ignore">Run the saved files</option>
          </select>
        </label>
        <label className="field">
          <span className="label">Job notifications</span>
          <select
            value={draft.notifications}
            onChange={(event) =>
              change('notifications', event.target.value as EditorPreferences['notifications'])
            }
          >
            <option value="failures">Failed jobs</option>
            <option value="all">All finished jobs</option>
            <option value="none">None</option>
          </select>
        </label>
        <label className="field">
          <span className="label">Recent job history</span>
          <input
            type="number"
            min="10"
            max="500"
            required
            value={Number.isFinite(draft.jobHistory) ? draft.jobHistory : ''}
            onChange={(event) => change('jobHistory', event.target.valueAsNumber)}
          />
        </label>
      </div>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={draft.problemMatching}
          onChange={(event) => change('problemMatching', event.target.checked)}
        />
        Show errors in the Problems panel
      </label>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={draft.testResults}
          onChange={(event) => change('testResults', event.target.checked)}
        />
        Show JUnit results in the Testing view
      </label>
      <div className="form-actions">
        <Button
          type="submit"
          icon="save"
          busy={busy['settings:preferences']}
          disabled={JSON.stringify(draft) === saved || !Number.isFinite(draft.jobHistory)}
        >
          Save preferences
        </Button>
        <span className="hint">Workspace overrides continue to apply.</span>
      </div>
    </form>
  );
}

function ClusterPreferences({
  cluster,
  admin,
  busy,
}: {
  cluster: ClusterSettings;
  admin: boolean;
  busy: Busy;
}) {
  const [draft, setDraft] = useState(cluster);
  const saved = JSON.stringify(cluster);
  useEffect(() => setDraft(JSON.parse(saved) as ClusterSettings), [saved]);
  const window = draft.updateWindow ?? { start: '22:00', end: '06:00' };
  return (
    <form
      className="form settings-form"
      onSubmit={(event) => {
        event.preventDefault();
        send({
          type: 'saveClusterSettings',
          patch: {
            ...draft,
            updateWindow: draft.autoUpdate === 'window' ? window : draft.updateWindow,
          },
        });
      }}
    >
      <fieldset disabled={!admin || busy['settings:cluster']} className="settings-fieldset form">
        <legend className="sr-only">Cluster configuration</legend>
        <div className="settings-fields">
          {(
            [
              ['jobRetentionDays', 'Keep jobs (days)'],
              ['logRetentionDays', 'Keep logs (days)'],
              ['snapshotRetentionDays', 'Keep unused snapshots (days)'],
            ] as const
          ).map(([key, label]) => (
            <label className="field" key={key}>
              <span className="label">{label}</span>
              <input
                type="number"
                min="1"
                max="3650"
                required
                value={Number.isFinite(draft[key]) ? draft[key] : ''}
                onChange={(event) =>
                  setDraft((previous) => ({ ...previous, [key]: event.target.valueAsNumber }))
                }
              />
            </label>
          ))}
          <label className="field">
            <span className="label">Worker auto-updates</span>
            <select
              aria-label="Worker auto-updates"
              value={draft.autoUpdate}
              onChange={(event) =>
                setDraft((previous) => ({
                  ...previous,
                  autoUpdate: event.target.value as ClusterSettings['autoUpdate'],
                }))
              }
            >
              <option value="follow">Follow the controller</option>
              <option value="window">During the update window</option>
              <option value="off">Off</option>
            </select>
          </label>
          {draft.autoUpdate === 'window' ? (
            <>
              <label className="field">
                <span className="label">Update window start</span>
                <input
                  type="time"
                  required
                  value={window.start}
                  onChange={(event) =>
                    setDraft((previous) => ({
                      ...previous,
                      updateWindow: { ...window, start: event.target.value },
                    }))
                  }
                />
              </label>
              <label className="field">
                <span className="label">Update window end</span>
                <input
                  type="time"
                  required
                  value={window.end}
                  onChange={(event) =>
                    setDraft((previous) => ({
                      ...previous,
                      updateWindow: { ...window, end: event.target.value },
                    }))
                  }
                />
              </label>
              <p className="hint settings-wide">Times use the controller’s local time zone.</p>
            </>
          ) : null}
          {(['act', 'node'] as const).map((tool) => (
            <label className="field" key={tool}>
              <span className="label">{tool === 'node' ? 'Node.js' : 'act'} version override</span>
              <input
                value={draft.toolchain[tool] ?? ''}
                placeholder="Use the build’s pinned version"
                pattern="([0-9]+\.[0-9]+\.[0-9]+|off)?"
                onChange={(event) =>
                  setDraft((previous) => ({
                    ...previous,
                    toolchain: { ...previous.toolchain, [tool]: event.target.value || null },
                  }))
                }
              />
              <span className="hint">A version such as 24.11.0, or “off” to stop managing it.</span>
            </label>
          ))}
        </div>
        <div className="form-actions">
          <Button
            type="submit"
            icon="save"
            busy={busy['settings:cluster']}
            disabled={JSON.stringify(draft) === saved}
          >
            Save cluster settings
          </Button>
        </div>
      </fieldset>
    </form>
  );
}

function AccessTokens({
  settings,
  data,
  admin,
  busy,
  createdToken,
  dismissToken,
}: {
  settings: SettingsView;
  data: DashboardState;
  admin: boolean;
  busy: Busy;
  createdToken: CreateTokenResponse | null;
  dismissToken: () => void;
}) {
  const [name, setName] = useState('');
  const [kind, setKind] = useState<'client' | 'join'>('client');
  const [role, setRole] = useState<'operator' | 'admin'>('operator');
  const [ttl, setTtl] = useState('');
  const [uses, setUses] = useState('1');
  const quote = (value: string) => `'${value.replace(/'/g, "'\\''")}'`;
  const controllerUrl = data.connection.publicUrl || data.connection.url;
  const command =
    createdToken && controllerUrl && data.connection.caFingerprint
      ? `gridstator ${createdToken.info.kind === 'join' ? 'agent join' : 'login'} ${quote(controllerUrl)} --fingerprint ${quote(data.connection.caFingerprint)} --token ${quote(createdToken.token)}`
      : null;
  return (
    <div className="stack">
      {createdToken ? (
        <div className="token-secret" role="status">
          <strong>Save the token for {createdToken.info.name}</strong>
          <p>This is the only time its secret is shown.</p>
          <code className="code-block">{createdToken.token}</code>
          {command ? (
            <>
              <p>
                Run this on the {createdToken.info.kind === 'join' ? 'worker' : 'client'} machine:
              </p>
              <code className="code-block">{command}</code>
            </>
          ) : null}
          <div className="form-actions">
            {command ? (
              <Button icon="terminal" onClick={() => send({ type: 'copy', text: command })}>
                Copy {createdToken.info.kind === 'join' ? 'join' : 'login'} command
              </Button>
            ) : null}
            <Button icon="copy" onClick={() => send({ type: 'copy', text: createdToken.token })}>
              Copy token
            </Button>
            <Button kind="secondary" onClick={dismissToken}>
              Dismiss token
            </Button>
          </div>
        </div>
      ) : null}
      {settings.tokens?.length ? (
        <div
          className="settings-table-wrap"
          tabIndex={0}
          role="region"
          aria-label="Access token details"
        >
          <table className="settings-table" aria-label="Access tokens">
            <thead>
              <tr>
                <th>Name</th>
                <th>Access</th>
                <th>Last used</th>
                <th>Expires</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {settings.tokens.map((token) => (
                <tr key={token.id}>
                  <th scope="row">{token.name}</th>
                  <td>{token.kind === 'client' ? token.role : token.kind}</td>
                  <td>{ago(token.lastUsedAt)}</td>
                  <td>
                    {token.revokedAt
                      ? 'Revoked'
                      : token.expiresAt
                        ? new Date(token.expiresAt).toLocaleString()
                        : 'Never'}
                  </td>
                  <td>
                    <Button
                      kind="ghost"
                      disabled={!admin || token.revokedAt !== null}
                      busy={busy['settings:tokens']}
                      onClick={() => send({ type: 'revokeToken', id: token.id })}
                      title={`Revoke ${token.name}`}
                    >
                      Revoke
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="muted">
          {admin ? 'No tokens to display.' : 'Admin access is required to view and manage tokens.'}
        </p>
      )}
      <form
        className="form settings-form"
        onSubmit={(event) => {
          event.preventDefault();
          send({
            type: 'createToken',
            request: {
              kind,
              name: name.trim(),
              role,
              ...(ttl ? { ttlSeconds: Number(ttl) * 3600 } : {}),
              ...(kind === 'join' ? { maxUses: Number(uses) } : {}),
            },
          });
        }}
      >
        <fieldset disabled={!admin || busy['settings:tokens']} className="settings-fieldset form">
          <legend className="sr-only">Create an access token</legend>
          <div className="settings-fields">
            <label className="field">
              <span className="label">Token name</span>
              <input
                maxLength={64}
                required
                value={name}
                onChange={(event) => setName(event.target.value)}
              />
            </label>
            <label className="field">
              <span className="label">Token type</span>
              <select
                value={kind}
                onChange={(event) => setKind(event.target.value as 'client' | 'join')}
              >
                <option value="client">Client sign-in</option>
                <option value="join">Join a worker</option>
              </select>
            </label>
            {kind === 'client' ? (
              <label className="field">
                <span className="label">Client role</span>
                <select
                  value={role}
                  onChange={(event) => setRole(event.target.value as 'operator' | 'admin')}
                >
                  <option value="operator">Operator</option>
                  <option value="admin">Admin</option>
                </select>
              </label>
            ) : (
              <label className="field">
                <span className="label">Worker joins allowed</span>
                <input
                  type="number"
                  min="1"
                  max="1000"
                  required
                  value={uses}
                  onChange={(event) => setUses(event.target.value)}
                />
              </label>
            )}
            <label className="field">
              <span className="label">Token lifetime (hours)</span>
              <input
                type="number"
                min="1"
                step="1"
                value={ttl}
                placeholder={kind === 'join' ? '24 hours' : 'Never expires'}
                onChange={(event) => setTtl(event.target.value)}
              />
            </label>
          </div>
          <div>
            <Button type="submit" icon="add" disabled={!name.trim()} busy={busy['settings:tokens']}>
              Create token
            </Button>
          </div>
        </fieldset>
      </form>
    </div>
  );
}

function Versions({ data, settings }: { data: DashboardState; settings: SettingsView }) {
  const controller = data.connection;
  const extensionDiffers =
    !!controller.version && controller.version !== settings.extension.version;
  return (
    <div
      className="settings-table-wrap"
      tabIndex={0}
      role="region"
      aria-label="Component version details"
    >
      <table className="settings-table" aria-label="Component versions">
        <thead>
          <tr>
            <th>Component</th>
            <th>Version</th>
            <th>Build</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <th scope="row">VS Code extension</th>
            <td>{settings.extension.version}</td>
            <td>
              <code>{settings.extension.build}</code>
            </td>
            <td className={extensionDiffers ? 'warn-text' : undefined}>
              {extensionDiffers ? 'Version differs from controller' : 'Installed'}
            </td>
          </tr>
          <tr>
            <th scope="row">
              Controller{controller.controller ? ` · ${controller.controller.hostname}` : ''}
            </th>
            <td>{controller.version ?? 'Unknown'}</td>
            <td>
              <code>{controller.build?.id ?? 'Not reported'}</code>
            </td>
            <td>{controller.status}</td>
          </tr>
          {data.nodes.map((node) => {
            const differs =
              node.update?.state === 'behind' ||
              (!!controller.build &&
                !!node.info?.build &&
                controller.build.id !== node.info.build.id);
            return (
              <tr key={node.id}>
                <th scope="row">Worker · {node.name}</th>
                <td>{node.info?.agentVersion ?? 'Unknown'}</td>
                <td>
                  <code>{node.update?.build ?? node.info?.build?.id ?? 'Not reported'}</code>
                </td>
                <td className={differs ? 'warn-text' : undefined}>
                  {node.status !== 'online'
                    ? 'Offline · last reported'
                    : differs
                      ? 'Different controller build'
                      : (node.update?.state ?? 'Reported')}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function SoftwareUpdates({
  settings,
  admin,
  busy,
}: {
  settings: SettingsView;
  admin: boolean;
  busy: Busy;
}) {
  const [tag, setTag] = useState('');
  const [prereleases, setPrereleases] = useState(settings.includePrereleases);
  const selected = settings.releases.find((release) => release.tag === tag) ?? settings.releases[0];
  const build = settings.updates?.controller;
  const canUpdateController = build?.selfUpdate?.available ?? false;
  const rollout = settings.updates?.rollout;
  return (
    <div className="stack">
      <div className="settings-subheading">
        <h3>Controller releases</h3>
        <Button
          kind="secondary"
          icon="refresh"
          busy={busy['settings:releases']}
          onClick={() => send({ type: 'requestReleases', includePrereleases: prereleases })}
        >
          Check for releases
        </Button>
      </div>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={prereleases}
          onChange={(event) => {
            setPrereleases(event.target.checked);
            send({ type: 'requestReleases', includePrereleases: event.target.checked });
          }}
        />
        Include prereleases
      </label>
      {settings.releaseError ? (
        <p className="error-text" role="alert">
          {settings.releaseError}
        </p>
      ) : null}
      {selected ? (
        <>
          <label className="field settings-form">
            <span className="label">Controller release</span>
            <select value={selected.tag} onChange={(event) => setTag(event.target.value)}>
              {settings.releases.map((release) => (
                <option key={release.tag} value={release.tag}>
                  {release.name || release.tag}
                  {release.prerelease ? ' · prerelease' : ''}
                </option>
              ))}
            </select>
          </label>
          <p className="muted settings-description">
            Published {new Date(selected.publishedAt).toLocaleDateString()} ·{' '}
            <a href={selected.url} className="link">
              {selected.tag} on GitHub
            </a>
          </p>
          {selected.notes ? (
            <details className="release-notes">
              <summary>Release notes</summary>
              <pre>{selected.notes}</pre>
            </details>
          ) : null}
        </>
      ) : (
        <p className="muted">
          Check the official Gridstator releases to choose a controller update.
        </p>
      )}
      <div className="form-actions">
        <Button
          icon="cloud-download"
          disabled={!admin || !selected || !canUpdateController}
          busy={busy['settings:install']}
          onClick={() => {
            if (selected) send({ type: 'installRelease', tag: selected.tag });
          }}
        >
          Install selected release
        </Button>
        <Button
          kind="secondary"
          icon="folder-opened"
          disabled={!admin || !canUpdateController}
          busy={busy['settings:install']}
          onClick={() => send({ type: 'uploadControllerBuild' })}
        >
          Upload a package…
        </Button>
      </div>
      <p className="hint">
        Downloads are checked against the published checksum. Installation restarts the controller;
        your windows reconnect automatically.
      </p>
      {!canUpdateController ? (
        <p className="hint">
          {build?.selfUpdate?.reason ?? 'Controller update capability has not been reported.'}
        </p>
      ) : null}
      <div className="settings-update-workers">
        <h3>Worker rollout</h3>
        <p className="muted">
          Workers update one at a time after their running jobs finish. New jobs wait or run on
          other workers.
        </p>
        <div className="form-actions">
          <Button
            icon="sync"
            disabled={!admin || !build?.updatable || rollout?.state === 'running'}
            busy={busy['settings:rollout']}
            onClick={() =>
              send({ type: 'startRollout', request: { now: false, drainTimeoutSeconds: null } })
            }
          >
            Update workers
          </Button>
          <Button
            kind="secondary"
            disabled={!admin || rollout?.state !== 'running'}
            busy={busy['settings:rollout']}
            onClick={() => send({ type: 'cancelRollout' })}
          >
            Pause rollout
          </Button>
        </div>
        {rollout ? (
          <div className="rollout-progress" role="status">
            <p>
              <strong>{rollout.state}</strong> · target <code>{rollout.target.id}</code>
            </p>
            {rollout.haltReason ? <p className="error-text">{rollout.haltReason}</p> : null}
            <ul className="plain">
              {rollout.nodes.map((node) => (
                <li key={node.nodeId}>
                  <strong>{node.name}</strong> · {node.state}
                  {node.detail ? ` · ${node.detail}` : ''}
                </li>
              ))}
            </ul>
          </div>
        ) : (
          <p className="hint">No rollout is in progress.</p>
        )}
      </div>
      <div className="settings-subheading">
        <h3>VS Code extension</h3>
        <Button
          kind="secondary"
          icon="extensions"
          onClick={() => send({ type: 'openExtensionUpdates' })}
        >
          Extension updates
        </Button>
      </div>
    </div>
  );
}

function HostControls({ settings, busy }: { settings: SettingsView; busy: Busy }) {
  const host = settings.host;
  const [address, setAddress] = useState(host?.host ?? '');
  const [port, setPort] = useState(String(host?.port ?? 7300));
  const [url, setUrl] = useState(host?.url ?? '');
  const signature = JSON.stringify({ host: host?.host, port: host?.port, url: host?.url });
  useEffect(() => {
    const saved = JSON.parse(signature) as { host?: string; port?: number; url?: string };
    setAddress(saved.host ?? '');
    setPort(String(saved.port ?? 7300));
    setUrl(saved.url ?? '');
  }, [signature]);
  if (!host?.available)
    return (
      <p className="muted">
        {host?.reason ??
          'Open an SSH window on the controller as its owning account to manage host settings and services.'}
      </p>
    );
  const canManage = host.service.installed && host.service.canManage !== false;
  const dirty = address !== host.host || port !== String(host.port) || url !== host.url;
  return (
    <div className="stack">
      <dl className="props compact">
        <dt>Host</dt>
        <dd>{host.hostname}</dd>
        <dt>Service</dt>
        <dd>{host.service.installed ? `${host.service.state} · user service` : 'Not installed'}</dd>
        <dt>Data directory</dt>
        <dd>
          <code>{host.dataDir}</code>
        </dd>
      </dl>
      <div className="form-actions">
        <Button
          icon="play"
          disabled={!canManage || host.service.state === 'active'}
          busy={busy['settings:host']}
          onClick={() => send({ type: 'hostService', action: 'start' })}
        >
          Start controller
        </Button>
        <Button
          kind="secondary"
          icon="debug-restart"
          disabled={!canManage || host.service.state !== 'active'}
          busy={busy['settings:host']}
          onClick={() => send({ type: 'hostService', action: 'restart' })}
        >
          Restart controller
        </Button>
        <Button
          kind="danger"
          icon="debug-stop"
          disabled={!canManage || host.service.state !== 'active'}
          busy={busy['settings:host']}
          onClick={() => send({ type: 'hostService', action: 'stop' })}
        >
          Stop controller
        </Button>
      </div>
      <p className="hint">A stopped controller stays stopped until you start it.</p>
      {host.service.reason ? <p className="hint">{host.service.reason}</p> : null}
      <form
        className="form settings-form"
        onSubmit={(event) => {
          event.preventDefault();
          send({
            type: 'saveHostConfiguration',
            host: address.trim(),
            port: Number(port),
            url: url.trim(),
          });
        }}
      >
        <div className="settings-fields">
          <label className="field">
            <span className="label">Listening address</span>
            <input required value={address} onChange={(event) => setAddress(event.target.value)} />
            <span className="hint">Use 0.0.0.0 to listen on all IPv4 interfaces.</span>
          </label>
          <label className="field">
            <span className="label">Listening port</span>
            <input
              type="number"
              min="1"
              max="65535"
              required
              value={port}
              onChange={(event) => setPort(event.target.value)}
            />
          </label>
          <label className="field settings-wide">
            <span className="label">Controller URL for workers</span>
            <input
              type="url"
              pattern="https://.*"
              required
              value={url}
              placeholder="https://192.168.1.77:7300"
              onChange={(event) => setUrl(event.target.value)}
            />
            <span className="hint">An HTTPS address reachable by every worker.</span>
          </label>
        </div>
        <div className="form-actions">
          <Button
            type="submit"
            icon="debug-restart"
            disabled={!dirty || !canManage}
            busy={busy['settings:host']}
          >
            Apply &amp; restart
          </Button>
        </div>
        <p className="hint">
          The move updates shared windows and worker connections, verifies reconnection, and
          recovers if verification fails.
        </p>
      </form>
      {host.aliases.length ? (
        <p className="hint">
          Previous listeners kept for reconnecting workers:{' '}
          {host.aliases.map((alias) => `${alias.host}:${alias.port}`).join(', ')}.
        </p>
      ) : null}
    </div>
  );
}

export function Settings({
  data,
  busy,
  createdToken,
  dismissToken,
}: {
  data: DashboardState;
  busy: Busy;
  createdToken: CreateTokenResponse | null;
  dismissToken: () => void;
}) {
  useEffect(() => {
    send({ type: 'refreshSettings' });
  }, []);
  const settings = data.settings;
  if (!settings)
    return (
      <Empty icon="settings-gear" title="Loading settings">
        <Button kind="secondary" onClick={() => send({ type: 'refreshSettings' })}>
          Refresh settings
        </Button>
      </Empty>
    );
  const c = data.connection;
  const admin = c.status === 'connected' && c.role === 'admin';
  return (
    <div className="settings-layout">
      <nav className="settings-nav" aria-label="Settings sections">
        {[
          ['connection', 'Connection'],
          ['preferences', 'Editor preferences'],
          ['versions', 'Versions'],
          ['cluster', 'Cluster'],
          ['tokens', 'Access tokens'],
          ['updates', 'Software updates'],
          ['host', 'Controller host'],
        ].map(([key, label]) => (
          <a key={key} href={`#settings-${key}`}>
            {label}
          </a>
        ))}
      </nav>
      <div className="settings-content">
        <Section
          id="connection"
          title="Connection"
          description="Your login is shared by the CLI and VS Code windows running under this account."
        >
          <dl className="props settings-connection">
            <dt>Controller</dt>
            <dd>
              {c.controller?.hostname ??
                (settings.host?.available ? settings.host.hostname : 'Not identified')}
            </dd>
            <dt>URL</dt>
            <dd>
              <code>{c.url ?? settings.host?.url ?? 'Not configured'}</code>
            </dd>
            <dt>Status</dt>
            <dd>{c.status}</dd>
            <dt>Signed in as</dt>
            <dd>{c.user ? `${c.user} · ${c.role ?? 'unknown role'}` : 'Not signed in'}</dd>
            <dt>Login source</dt>
            <dd>
              <code>{c.source ?? c.configPath ?? 'Default client configuration'}</code>
            </dd>
          </dl>
          {c.error ? (
            <p className="error-text" role="alert">
              {c.error}
            </p>
          ) : null}
          <div className="form-actions">
            <Button kind="secondary" icon="refresh" onClick={() => send({ type: 'refresh' })}>
              Reconnect
            </Button>
            {c.canRestoreLocalAccess && !admin ? (
              <Button
                icon="key"
                busy={busy['settings:restore']}
                onClick={() => send({ type: 'restoreLocalAccess' })}
              >
                Restore local access
              </Button>
            ) : null}
            <Button kind="ghost" icon="sign-in" onClick={() => send({ type: 'signIn' })}>
              Sign in to another controller…
            </Button>
          </div>
          {settings.error ? (
            <p className="error-text" role="alert">
              {settings.error}
            </p>
          ) : null}
        </Section>
        <Section
          id="preferences"
          title="Editor preferences"
          description="Choose how Gridstator works in VS Code."
        >
          <Preferences settings={settings} busy={busy} />
        </Section>
        <Section
          id="versions"
          title="Versions"
          description="Compare the running controller and worker builds, and your installed extension."
        >
          <Versions data={data} settings={settings} />
        </Section>
        <Section
          id="cluster"
          title="Cluster"
          description="These settings apply to every client and worker in this cluster."
        >
          {!admin ? (
            <p className="hint">Connect with admin access to change cluster settings.</p>
          ) : null}
          {settings.cluster ? (
            <ClusterPreferences cluster={settings.cluster} admin={admin} busy={busy} />
          ) : (
            <p className="muted">Connect to the controller to load its configuration.</p>
          )}
        </Section>
        <Section
          id="tokens"
          title="Access tokens"
          description="Create named client logins and worker join tokens, or revoke access."
        >
          <AccessTokens
            settings={settings}
            data={data}
            admin={admin}
            busy={busy}
            createdToken={createdToken}
            dismissToken={dismissToken}
          />
        </Section>
        <Section
          id="updates"
          title="Software updates"
          description="Install an official release or a package, then follow the worker rollout."
        >
          <SoftwareUpdates settings={settings} admin={admin} busy={busy} />
        </Section>
        <Section
          id="host"
          title="Controller host"
          description="Manage the controller’s service and listening address from its own host."
        >
          <HostControls settings={settings} busy={busy} />
        </Section>
      </div>
    </div>
  );
}

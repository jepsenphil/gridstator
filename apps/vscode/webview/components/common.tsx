import type { ReactNode } from 'react';
import type { Job, JobState, Node } from '@gridstator/protocol';
import { cx } from '../util';

export function Icon({
  name,
  spin,
  className,
  title,
}: {
  name: string;
  spin?: boolean;
  className?: string;
  title?: string;
}) {
  return (
    <i
      className={cx('codicon', `codicon-${name}`, spin && 'codicon-modifier-spin', className)}
      aria-hidden={title ? undefined : true}
      title={title}
    />
  );
}

const STATE_ICON: Record<JobState, string> = {
  created: 'circle-outline',
  queued: 'clock',
  assigned: 'loading',
  starting: 'loading',
  running: 'sync',
  succeeded: 'pass-filled',
  failed: 'error',
  cancelled: 'circle-slash',
  lost: 'warning',
};

export function isActive(state: JobState): boolean {
  return state === 'assigned' || state === 'starting' || state === 'running';
}

export function isTerminalState(state: JobState): boolean {
  return state === 'succeeded' || state === 'failed' || state === 'cancelled' || state === 'lost';
}

export function StateIcon({ state }: { state: JobState }) {
  return (
    <Icon
      name={STATE_ICON[state]}
      spin={isActive(state)}
      className={`state-${state}`}
      title={state}
    />
  );
}

export function StateBadge({ state }: { state: JobState }) {
  return (
    <span className={cx('badge', `badge-${state}`)}>
      <StateIcon state={state} /> {state}
    </span>
  );
}

export function Button({
  children,
  onClick,
  kind = 'primary',
  icon,
  disabled,
  busy,
  title,
  type = 'button',
}: {
  children?: ReactNode;
  onClick?: () => void;
  kind?: 'primary' | 'secondary' | 'danger' | 'ghost';
  icon?: string;
  disabled?: boolean;
  busy?: boolean;
  title?: string;
  type?: 'button' | 'submit';
}) {
  return (
    <button
      type={type}
      className={cx('btn', `btn-${kind}`, !children && 'btn-icon')}
      onClick={onClick}
      disabled={disabled || busy}
      title={title}
      aria-label={children ? undefined : title}
    >
      {busy ? <Icon name="loading" spin /> : icon ? <Icon name={icon} /> : null}
      {children ? <span>{children}</span> : null}
    </button>
  );
}

export function Meter({
  value,
  max,
  label,
  warnAt = 0.75,
  danger = 0.9,
}: {
  value: number;
  max: number;
  label?: string;
  warnAt?: number;
  danger?: number;
}) {
  const ratio = max > 0 ? Math.min(1, Math.max(0, value / max)) : 0;
  return (
    <div
      className="meter"
      role="meter"
      aria-valuenow={Math.round(ratio * 100)}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={label}
    >
      <div
        className={cx(
          'meter-fill',
          ratio >= danger ? 'meter-danger' : ratio >= warnAt ? 'meter-warn' : '',
        )}
        style={{ width: `${ratio * 100}%` }}
      />
    </div>
  );
}

export function Empty({
  icon,
  title,
  children,
}: {
  icon: string;
  title: string;
  children?: ReactNode;
}) {
  return (
    <div className="empty">
      <Icon name={icon} />
      <div className="empty-title">{title}</div>
      {children ? <div className="empty-body">{children}</div> : null}
    </div>
  );
}

export function Card({
  title,
  actions,
  children,
  className,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cx('card', className)}>
      {title || actions ? (
        <header className="card-header">
          {title ? <h3>{title}</h3> : <span />}
          {actions ? <div className="card-actions">{actions}</div> : null}
        </header>
      ) : null}
      <div className="card-body">{children}</div>
    </section>
  );
}

export function nodeStatus(n: Node): { label: string; className: string; icon: string } {
  if (n.status !== 'online')
    return { label: 'offline', className: 'muted', icon: 'debug-disconnect' };
  if (n.health === 'degraded') return { label: 'degraded', className: 'warn', icon: 'warning' };
  if (n.settings.schedulingMode === 'draining')
    return { label: 'draining', className: 'warn', icon: 'debug-pause' };
  if (n.settings.schedulingMode === 'disabled')
    return { label: 'disabled', className: 'warn', icon: 'circle-slash' };
  return { label: 'online', className: 'ok', icon: 'vm-running' };
}

export function kindLabel(job: Job): string {
  return job.kind === 'docker-build' ? 'build' : job.kind === 'actions' ? 'actions' : 'command';
}

export function KindIcon({ kind }: { kind: string }) {
  return (
    <Icon
      name={kind === 'docker-build' ? 'package' : kind === 'actions' ? 'github-action' : 'terminal'}
      title={kind}
    />
  );
}

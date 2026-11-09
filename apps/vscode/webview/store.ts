import type { JobEvent, LogEntry } from '@gridstator/protocol';
import type { DashboardState, ToWebview, View } from '../src/messages';

export interface LogLine {
  seq: number;
  stream: 'stdout' | 'stderr' | 'system';
  text: string;
}

export interface JobLogs {
  lines: LogLine[];
  /** Streams whose last line is still open (no trailing newline yet). */
  open: { stdout: boolean; stderr: boolean; system: boolean };
  ended: boolean;
}

export interface Toast {
  id: number;
  level: 'info' | 'error';
  message: string;
}

export interface UiState {
  data: DashboardState | null;
  view: View;
  jobId: string | null;
  nodeId: string | null;
  itemId: string | null;
  logs: Record<string, JobLogs>;
  events: Record<string, JobEvent[]>;
  busy: Record<string, boolean>;
  toasts: Toast[];
}

export const initialState: UiState = {
  data: null,
  view: 'overview',
  jobId: null,
  nodeId: null,
  itemId: null,
  logs: {},
  events: {},
  busy: {},
  toasts: [],
};

export type Action =
  | ToWebview
  | {
      type: 'go';
      view: View;
      jobId?: string | null;
      nodeId?: string | null;
      itemId?: string | null;
    }
  | { type: 'dismissToast'; id: number };

const MAX_LINES = 200_000;
let toastId = 0;

function appendLogs(prev: JobLogs | undefined, entries: LogEntry[], reset: boolean): JobLogs {
  const base: JobLogs =
    reset || !prev
      ? { lines: [], open: { stdout: false, stderr: false, system: false }, ended: false }
      : prev;
  const lines = base.lines.slice();
  const open = { ...base.open };
  for (const e of entries) {
    const parts = e.d.split('\n');
    const endsWithNewline = e.d.endsWith('\n');
    if (endsWithNewline) parts.pop();
    parts.forEach((text, i) => {
      if (i === 0 && open[e.o]) {
        // Continue the previous open line of this stream.
        for (let j = lines.length - 1; j >= 0; j--) {
          if (lines[j]!.stream === e.o) {
            lines[j] = { ...lines[j]!, text: lines[j]!.text + text };
            return;
          }
        }
      }
      lines.push({ seq: e.s, stream: e.o, text });
    });
    open[e.o] = !endsWithNewline;
  }
  return {
    lines: lines.length > MAX_LINES ? lines.slice(lines.length - MAX_LINES) : lines,
    open,
    ended: base.ended,
  };
}

export function reducer(state: UiState, action: Action): UiState {
  switch (action.type) {
    case 'state':
      return { ...state, data: action.state };
    case 'navigate':
    case 'go':
      return {
        ...state,
        view: action.view,
        jobId: action.jobId ?? (action.view === 'job' ? state.jobId : state.jobId),
        nodeId: action.nodeId ?? null,
        itemId: action.itemId ?? (action.view === 'run' ? state.itemId : null),
      };
    case 'logs':
      return {
        ...state,
        logs: {
          ...state.logs,
          [action.jobId]: appendLogs(state.logs[action.jobId], action.entries, action.reset),
        },
      };
    case 'logs.end': {
      const prev = state.logs[action.jobId];
      return {
        ...state,
        logs: {
          ...state.logs,
          [action.jobId]: {
            ...(prev ?? { lines: [], open: { stdout: false, stderr: false, system: false } }),
            ended: true,
          },
        },
      };
    }
    case 'events':
      return { ...state, events: { ...state.events, [action.jobId]: action.events } };
    case 'busy':
      return { ...state, busy: { ...state.busy, [action.key]: action.busy } };
    case 'toast':
      return {
        ...state,
        toasts: [
          ...state.toasts.slice(-3),
          { id: ++toastId, level: action.level, message: action.message },
        ],
      };
    case 'dismissToast':
      return { ...state, toasts: state.toasts.filter((t) => t.id !== action.id) };
  }
}

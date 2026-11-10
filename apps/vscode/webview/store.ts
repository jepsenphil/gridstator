import type { CreateTokenResponse, JobEvent, LogEntry } from '@gridstator/protocol';
import type { TestSummary } from '@gridstator/reports';
import type { DashboardState, ToWebview, View, WorkerJoinCommand } from '../src/messages';

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
  groupId: string | null;
  /** Combined test results per group: undefined = not loaded, null = no reports. */
  groupTests: Record<string, TestSummary | null>;
  logs: Record<string, JobLogs>;
  events: Record<string, JobEvent[]>;
  busy: Record<string, boolean>;
  toasts: Toast[];
  /** Shown once after creation; never persisted with the webview's navigation state. */
  createdToken: CreateTokenResponse | null;
  workerJoinCommand: WorkerJoinCommand | null;
}

export const initialState: UiState = {
  data: null,
  view: 'overview',
  jobId: null,
  nodeId: null,
  itemId: null,
  groupId: null,
  groupTests: {},
  logs: {},
  events: {},
  busy: {},
  toasts: [],
  createdToken: null,
  workerJoinCommand: null,
};

export type Action =
  | ToWebview
  | {
      type: 'go';
      view: View;
      jobId?: string | null;
      nodeId?: string | null;
      itemId?: string | null;
      groupId?: string | null;
    }
  | { type: 'dismissToast'; id: number }
  | { type: 'dismissToken' }
  | { type: 'dismissWorkerJoinCommand' };

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
    case 'tokenCreated':
      return { ...state, createdToken: { token: action.token, info: action.info } };
    case 'dismissToken':
      return { ...state, createdToken: null };
    case 'workerJoinCommandCreated':
      return action.result.clusterId === state.data?.connection.clusterId
        ? { ...state, workerJoinCommand: action.result }
        : state;
    case 'dismissWorkerJoinCommand':
      return { ...state, workerJoinCommand: null };
    case 'state':
      return {
        ...state,
        data: action.state,
        createdToken:
          state.data?.connection.clusterId === action.state.connection.clusterId
            ? state.createdToken
            : null,
        workerJoinCommand:
          state.data?.connection.clusterId === action.state.connection.clusterId
            ? state.workerJoinCommand
            : null,
      };
    case 'navigate':
    case 'go':
      return {
        ...state,
        view: action.view,
        jobId: action.jobId ?? (action.view === 'job' ? state.jobId : state.jobId),
        nodeId: action.nodeId ?? null,
        itemId: action.itemId ?? (action.view === 'run' ? state.itemId : null),
        groupId: action.groupId ?? (action.view === 'group' ? state.groupId : null),
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
    case 'groupTests':
      return { ...state, groupTests: { ...state.groupTests, [action.groupId]: action.summary } };
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

import { invoke } from '@tauri-apps/api/core';

export interface Budget {
  cpus?: number | null;
  memoryMb?: number | null;
  maxConcurrency?: number | null;
}

export interface Job {
  id: string;
  name: string;
  phase: string;
  startedAt?: string | number;
}

export interface Worker {
  id: string;
  label: string;
  distribution?: string;
  linuxUser?: string;
  dataDir: string;
  nodeId: string;
  clusterId: string | null;
  caFingerprint?: string;
  name: string;
  controllerUrl: string;
  embedded: boolean;
  running: boolean;
  connected: boolean;
  state: string;
  paused: boolean;
  draining: boolean;
  startAtSignIn: boolean;
  budget: Budget;
  controllerHold?: string | null;
  controllerMode?: string;
  effectiveBudget?: Budget;
  controllerSettings?: {
    schedulingMode?: string;
    maxConcurrency?: number | null;
    reserved?: { cpus?: number; memoryMb?: number };
  };
  controllerRunning?: boolean;
  generation: number;
  activeJobs: Job[];
  recentJobs: Job[];
  info?: string | Record<string, unknown>;
  error?: string;
}

export interface Snapshot {
  platform: 'linux' | 'macos' | 'windows';
  workers: Worker[];
  wslDistributions: {
    name: string;
    version: 1 | 2;
    state: 'running' | 'stopped' | 'unavailable';
  }[];
  settings: { startAtSignIn: boolean; nativeCliPath?: string };
  nativeAvailable: boolean;
  error?: string;
}

export type WorkerAction =
  | { action: 'pause' | 'resume' | 'start' | 'stop'; workerId: string }
  | { action: 'forceStop'; workerId: string; confirmed: true }
  | { action: 'cancel'; workerId: string; jobId: string; confirmed: true }
  | { action: 'setBudget'; workerId: string; budget: Budget }
  | { action: 'setStartup'; workerId: string; startAtSignIn: boolean };

export interface SetupRequest {
  mode: 'join' | 'create';
  distribution?: string;
  linuxUser?: string;
  connection?: string;
  controllerUrl?: string;
  caFingerprint?: string;
  token?: string;
  name?: string;
}

export interface Logs {
  jobId: string;
  entries: { s: number; t: string | number; o: string; d: string }[];
  nextSeq: number;
}

// Commands are intentionally narrow. The webview has no shell, filesystem or network privileges.
export const desktop = {
  snapshot: () => invoke<Snapshot>('desktop_snapshot'),
  action: (request: WorkerAction) => invoke<Snapshot>('desktop_action', { request }),
  logs: (request: { workerId: string; jobId: string; afterSeq?: number; limit?: number }) =>
    invoke<Logs>('desktop_logs', { request }),
  setup: (request: SetupRequest) => invoke<Snapshot>('desktop_setup', { request }),
  install: (request: { distribution?: string; linuxUser?: string }) =>
    invoke<Snapshot>('desktop_install', { request }),
  settings: (request: { startAtSignIn?: boolean; nativeCliPath?: string }) =>
    invoke<Snapshot>('desktop_settings', { request }),
};

export function errorMessage(error: unknown): string {
  return typeof error === 'string'
    ? error
    : error instanceof Error
      ? error.message
      : 'The local worker could not complete this action. Refresh its status and try again.';
}

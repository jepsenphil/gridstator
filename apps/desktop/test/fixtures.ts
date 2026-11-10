import type { Snapshot, Worker } from '../src/api.js';

export const worker: Worker = {
  id: 'local:workstation',
  label: 'Workstation',
  dataDir: '/home/test/.local/share/gridstator-v2',
  nodeId: 'node-workstation',
  clusterId: 'cluster-dev',
  name: 'Workstation',
  controllerUrl: 'https://controller.example:7443',
  embedded: true,
  controllerRunning: true,
  running: true,
  connected: true,
  state: 'accepting',
  paused: false,
  draining: false,
  startAtSignIn: true,
  budget: { cpus: 6, memoryMb: 8192, maxConcurrency: 3 },
  generation: 1,
  controllerSettings: {
    schedulingMode: 'enabled',
    maxConcurrency: 4,
    reserved: { cpus: 2, memoryMb: 2048 },
  },
  effectiveBudget: { cpus: 6, memoryMb: 8192, maxConcurrency: 3 },
  activeJobs: [
    {
      id: 'job-test-suite',
      name: 'Run integration tests',
      phase: 'running',
      startedAt: '2026-11-10T09:32:00',
    },
    {
      id: 'job-build-image',
      name: 'Build app image',
      phase: 'starting',
      startedAt: '2026-11-10T09:35:00',
    },
  ],
  recentJobs: [
    {
      id: 'job-typecheck',
      name: 'Typecheck packages',
      phase: 'succeeded',
      startedAt: '2026-11-10T09:29:00',
    },
    {
      id: 'job-unit-tests',
      name: 'Run unit tests',
      phase: 'succeeded',
      startedAt: '2026-11-10T09:21:00',
    },
  ],
};

export const snapshot: Snapshot = {
  platform: 'linux',
  workers: [worker],
  wslDistributions: [],
  settings: { startAtSignIn: true },
  nativeAvailable: true,
};

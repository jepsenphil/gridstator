/**
 * Message contract between the extension host and the dashboard webview. The webview never talks
 * to the controller directly; every action goes through the extension, which owns the pinned-TLS
 * client and the user's workspace.
 */
import type { TestSummary } from '@gridstator/reports';
import type {
  Job,
  JobGroup,
  JobEvent,
  JobState,
  LogEntry,
  Node,
  NodeSettingsPatch,
  ClusterSettings,
  ClusterSettingsPatch,
  ControllerBuild,
  CreateTokenRequest,
  ReleaseView,
  StartUpdateRequest,
  TokenView,
  UpdatesView,
} from '@gridstator/protocol';

export type ConnectionStatus = 'signedOut' | 'connecting' | 'connected' | 'error';

export interface ConnectionInfo {
  status: ConnectionStatus;
  url: string | null;
  publicUrl?: string;
  user: string | null;
  role: string | null;
  clusterId: string | null;
  caFingerprint?: string;
  version: string | null;
  error: string | null;
  controller?: { hostname: string; machineId: string };
  build?: ControllerBuild;
  configPath?: string;
  source?: string;
  extensionVersion?: string;
  canRestoreLocalAccess?: boolean;
}

export interface EditorPreferences {
  autoConnect: boolean;
  configPath: string;
  defaultNode: string;
  unsavedFiles: 'prompt' | 'saveAll' | 'ignore';
  notifications: 'all' | 'failures' | 'none';
  problemMatching: boolean;
  testResults: boolean;
  jobHistory: number;
}

export interface LocalControllerHost {
  available: boolean;
  dataDir: string;
  hostname: string;
  host: string;
  port: number;
  url: string;
  service: {
    installed: boolean;
    state: 'active' | 'inactive' | 'failed' | 'unknown';
    scope: 'user';
    canManage?: boolean;
    reason?: string | null;
  };
  aliases: Array<{ host: string; port: number }>;
  clusterId?: string | null;
  caFingerprint?: string | null;
  reason?: string | null;
}

export interface SettingsView {
  preferences: EditorPreferences;
  cluster: ClusterSettings | null;
  updates: UpdatesView | null;
  tokens: TokenView[] | null;
  releases: ReleaseView[];
  includePrereleases: boolean;
  releaseError?: string | null;
  host: LocalControllerHost | null;
  extension: { version: string; build: string };
  error?: string | null;
}

export interface WorkflowInputInfo {
  name: string;
  description?: string;
  required: boolean;
  default?: string;
  type?: string;
  options?: string[];
}

export type LaunchKind = 'task' | 'build' | 'workflow' | 'script' | 'dockerfile' | 'command';

export interface LaunchItem {
  /** Stable identifier, e.g. "task:test" or "workflow:.github/workflows/ci.yml". */
  id: string;
  kind: LaunchKind;
  label: string;
  description?: string;
  /** Workspace folder (project) this item belongs to. */
  folder: string;
  warnings: string[];
  workflow?: {
    file: string;
    events: string[];
    defaultEvent: string;
    inputs: WorkflowInputInfo[];
    jobs: Array<{
      id: string;
      name: string;
      needs: string[];
      runnable: boolean;
      warnings: string[];
    }>;
  };
  dockerfile?: { file: string; context: string; stages: string[] };
  script?: {
    dir: string;
    packageName: string;
    script: string;
    command: string;
    packageManager: string;
  };
  task?: { name: string; command: string; junit?: string; shards?: number };
  build?: { name: string; stage?: string; tags: string[]; output: string };
}

export interface LaunchOptions {
  /** Node id or name; empty for automatic placement. */
  node?: string;
  /** Preferred nodes (soft): used when eligible. */
  prefer?: string[];
  /** Split into this many parallel shards (command-like items only). */
  shards?: number;
  cpus?: number;
  memoryGb?: number;
  env?: Record<string, string>;
  /** Workflows */
  job?: string;
  event?: string;
  inputs?: Record<string, string>;
  /** Docker builds */
  stage?: string;
  tags?: string[];
  output?: 'load' | 'push' | 'tar' | 'none';
  buildArgs?: Record<string, string>;
  /** Custom command */
  command?: string;
  image?: string;
}

export interface LaunchRequest {
  itemId: string;
  options: LaunchOptions;
}

export interface DashboardState {
  connection: ConnectionInfo;
  nodes: Node[];
  jobs: Job[];
  groups: JobGroup[];
  launcher: { items: LaunchItem[]; defaultNode: string; loading: boolean };
  /** Jobs submitted from this VS Code window (rerun and problem matching apply to these). */
  myJobIds: string[];
  settings?: SettingsView;
}

export type View = 'overview' | 'jobs' | 'run' | 'nodes' | 'job' | 'group' | 'settings';

export type ToWebview =
  | { type: 'tokenCreated'; token: string; info: TokenView }
  | { type: 'state'; state: DashboardState }
  | {
      type: 'navigate';
      view: View;
      jobId?: string;
      nodeId?: string;
      itemId?: string;
      groupId?: string;
    }
  | { type: 'groupTests'; groupId: string; summary: TestSummary | null }
  | { type: 'logs'; jobId: string; entries: LogEntry[]; reset: boolean }
  | { type: 'logs.end'; jobId: string; state: JobState }
  | { type: 'events'; jobId: string; events: JobEvent[] }
  | { type: 'busy'; key: string; busy: boolean }
  | { type: 'toast'; level: 'info' | 'error'; message: string };

export type FromWebview =
  | { type: 'refreshSettings' }
  | { type: 'savePreferences'; patch: Partial<EditorPreferences> }
  | { type: 'saveClusterSettings'; patch: ClusterSettingsPatch }
  | { type: 'createToken'; request: CreateTokenRequest }
  | { type: 'revokeToken'; id: string }
  | { type: 'requestReleases'; includePrereleases: boolean }
  | { type: 'installRelease'; tag: string }
  | { type: 'uploadControllerBuild' }
  | { type: 'startRollout'; request: StartUpdateRequest }
  | { type: 'cancelRollout' }
  | { type: 'cancelControllerInstall' }
  | { type: 'hostService'; action: 'start' | 'stop' | 'restart' }
  | { type: 'saveHostConfiguration'; host: string; port: number; url: string }
  | { type: 'restoreLocalAccess' }
  | { type: 'openExtensionUpdates' }
  | { type: 'ready' }
  | { type: 'selectJob'; jobId: string | null }
  | { type: 'cancel'; jobId: string }
  | { type: 'retry'; jobId: string }
  | { type: 'rerun'; jobId: string }
  | { type: 'cancelGroup'; groupId: string }
  | { type: 'retryGroup'; groupId: string }
  | { type: 'requestGroupTests'; groupId: string }
  | { type: 'downloadArtifact'; jobId: string; name: string }
  | { type: 'updateNode'; nodeId: string; patch: NodeSettingsPatch }
  | { type: 'launch'; request: LaunchRequest }
  | { type: 'setDefaultNode'; node: string }
  | { type: 'refresh' }
  | { type: 'signIn' }
  | { type: 'openFile'; jobId: string; path: string; line?: number; column?: number }
  | { type: 'copy'; text: string };

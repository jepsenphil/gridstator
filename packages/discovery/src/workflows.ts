import { parse as parseYaml } from 'yaml';

export interface DispatchInput {
  name: string;
  description?: string;
  required: boolean;
  default?: string;
  type?: string;
  options?: string[];
}

export interface WorkflowJobInfo {
  id: string;
  name: string;
  runsOn: string[];
  needs: string[];
  /** True when the job uses a strategy matrix (act expands all combinations). */
  matrix: boolean;
  /** Reusable workflow referenced by `jobs.<id>.uses`. */
  usesWorkflow?: string;
  /** False when act cannot run the job at all (e.g. Windows/macOS runners). */
  runnable: boolean;
  warnings: string[];
}

export interface WorkflowInfo {
  /** Path relative to the project root, e.g. ".github/workflows/ci.yml". */
  file: string;
  name: string;
  events: string[];
  /** The event Gridstator uses when none is specified. */
  defaultEvent: string;
  dispatchInputs: DispatchInput[];
  jobs: WorkflowJobInfo[];
  warnings: string[];
  parseError?: string;
}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === 'object' && v !== null && !Array.isArray(v);
const asStrings = (v: unknown): string[] =>
  typeof v === 'string'
    ? [v]
    : Array.isArray(v)
      ? v.filter((x): x is string => typeof x === 'string')
      : [];

function eventsOf(on: unknown): string[] {
  if (typeof on === 'string') return [on];
  if (Array.isArray(on)) return asStrings(on);
  if (isObj(on)) return Object.keys(on);
  return [];
}

function dispatchInputs(on: unknown): DispatchInput[] {
  if (!isObj(on) || !isObj(on['workflow_dispatch']) || !isObj(on['workflow_dispatch']['inputs']))
    return [];
  return Object.entries(on['workflow_dispatch']['inputs']).map(([name, raw]) => {
    const def = isObj(raw) ? raw : {};
    const input: DispatchInput = { name, required: def['required'] === true };
    if (typeof def['description'] === 'string') input.description = def['description'];
    const dflt = def['default'];
    if (typeof dflt === 'string' || typeof dflt === 'number' || typeof dflt === 'boolean') {
      input.default = String(dflt);
    }
    if (typeof def['type'] === 'string') input.type = def['type'];
    if (Array.isArray(def['options'])) input.options = def['options'].map(String);
    return input;
  });
}

function runsOnLabels(v: unknown): string[] {
  if (typeof v === 'string') return [v];
  if (Array.isArray(v)) return asStrings(v);
  if (isObj(v))
    return [
      ...asStrings(v['labels']),
      ...(typeof v['group'] === 'string' ? [`group:${v['group']}`] : []),
    ];
  return [];
}

const DOCKER_ACTIONS =
  /^docker\/(build-push-action|setup-buildx-action|login-action|bake-action|metadata-action)@/;

/** Best-effort detection of GitHub Actions features that act does not (fully) support. */
export function analyzeJob(id: string, job: Obj): WorkflowJobInfo {
  const warnings: string[] = [];
  const runsOn = runsOnLabels(job['runs-on']);
  let runnable = true;
  if (runsOn.some((l) => /windows|macos|mac-os|osx/i.test(l))) {
    runnable = false;
    warnings.push(
      `runs-on ${runsOn.join(', ')}: act runs Linux containers only, so this job cannot run`,
    );
  } else if (runsOn.includes('self-hosted')) {
    warnings.push(
      'self-hosted runner labels need an image mapping (actions.platforms in gridstator.yml)',
    );
  } else if (runsOn.some((l) => l.includes('${{'))) {
    warnings.push(
      'runs-on is an expression; act needs it to resolve to a label with an image mapping',
    );
  }
  const uses = typeof job['uses'] === 'string' ? job['uses'] : undefined;
  if (uses && !uses.startsWith('./')) {
    warnings.push(
      `calls remote reusable workflow ${uses}; act fetches it from GitHub (network and possibly a GITHUB_TOKEN secret needed)`,
    );
  }
  if (job['environment'] !== undefined)
    warnings.push(
      'deployment environments and their protection rules and secrets are not available locally',
    );
  const permissions = job['permissions'];
  if (isObj(permissions) && permissions['id-token'] === 'write')
    warnings.push('OIDC tokens (id-token: write) are not available locally');
  const steps = Array.isArray(job['steps']) ? job['steps'].filter(isObj) : [];
  let needsDocker = false;
  for (const step of steps) {
    const u = typeof step['uses'] === 'string' ? step['uses'] : '';
    const w = isObj(step['with']) ? step['with'] : {};
    if (
      u.startsWith('actions/checkout@') &&
      typeof w['repository'] === 'string' &&
      !w['repository'].includes('github.repository')
    ) {
      warnings.push(
        `actions/checkout of ${w['repository']} clones that repository from GitHub (your snapshot is used only for this repository)`,
      );
    }
    if (DOCKER_ACTIONS.test(u)) needsDocker = true;
    if (typeof step['run'] === 'string' && /(^|[\s;&|])docker\s/.test(step['run']))
      needsDocker = true;
  }
  if (needsDocker)
    warnings.push(
      'uses Docker inside the job; enable the Docker socket (actions.dockerSocket or --docker-socket)',
    );
  const needs = asStrings(job['needs']);
  const strategy = isObj(job['strategy']) ? job['strategy'] : undefined;
  return {
    id,
    name: typeof job['name'] === 'string' ? job['name'] : id,
    runsOn,
    needs,
    matrix: strategy !== undefined && strategy['matrix'] !== undefined,
    ...(uses ? { usesWorkflow: uses } : {}),
    runnable,
    warnings,
  };
}

export function parseWorkflow(file: string, text: string): WorkflowInfo {
  let doc: unknown;
  try {
    doc = parseYaml(text);
  } catch (err) {
    return {
      file,
      name: file,
      events: [],
      defaultEvent: 'push',
      dispatchInputs: [],
      jobs: [],
      warnings: [],
      parseError: (err as Error).message,
    };
  }
  if (!isObj(doc)) {
    return {
      file,
      name: file,
      events: [],
      defaultEvent: 'push',
      dispatchInputs: [],
      jobs: [],
      warnings: [],
      parseError: 'not a workflow (expected a mapping)',
    };
  }
  // YAML 1.1 parsers read `on:` as boolean true; the yaml library (1.2) keeps "on".
  const on = doc['on'] ?? (doc as Record<string, unknown>)['true'];
  const events = eventsOf(on);
  const jobsObj = isObj(doc['jobs']) ? doc['jobs'] : {};
  const jobs = Object.entries(jobsObj)
    .filter((entry): entry is [string, Obj] => isObj(entry[1]))
    .map(([id, job]) => analyzeJob(id, job));
  const warnings: string[] = [];
  if (/secrets\.GITHUB_TOKEN|github\.token/.test(text)) {
    warnings.push(
      'uses GITHUB_TOKEN; locally it is only set if you forward one (--secret-env GITHUB_TOKEN)',
    );
  }
  if (isObj(doc['permissions']) && doc['permissions']['id-token'] === 'write')
    warnings.push('OIDC tokens (id-token: write) are not available locally');
  if (jobs.length === 0) warnings.push('no jobs found');
  const defaultEvent = events.includes('push')
    ? 'push'
    : events.includes('workflow_dispatch')
      ? 'workflow_dispatch'
      : (events[0] ?? 'push');
  return {
    file,
    name: typeof doc['name'] === 'string' ? doc['name'] : file.split('/').pop()!,
    events,
    defaultEvent,
    dispatchInputs: dispatchInputs(on),
    jobs,
    warnings,
  };
}

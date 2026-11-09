import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import { join, posix } from 'node:path';
import { type Document, isMap, isScalar, isSeq, parseDocument } from 'yaml';

/**
 * act skips `actions/checkout` (and copies the snapshot into the job container instead) only when
 * the step's raw `with.ref` / `with.repository` text equals `github.ref` / `github.repository`.
 * It never evaluates expressions, so a step like `ref: ${{ inputs.source_ref || github.sha }}`
 * runs the real checkout: it needs a token, fetches from GitHub, and never sees the snapshot.
 *
 * Gridstator runs exactly the submitted working tree, so every checkout of the workflow's own
 * repository is pointed at the snapshot by removing `ref` (and a same-repository `repository`).
 * Checkouts of other repositories are left alone.
 *
 * act reads workflows from the same directory it copies into the job container, and jobs may
 * read their own workflow files (tests that assert on `.github/workflows`, for example). The
 * rewrite therefore goes into private copies under {@link ACT_WORKFLOWS_DIR}, which act parses
 * but the job never sees as workflow files. The developer's files and the job's
 * `.github/workflows` stay exactly as written.
 */
export interface CheckoutRewrite {
  /** Workflow path relative to the snapshot root. */
  file: string;
  job: string;
  step: string;
  /** Inputs removed from the step, with their original text. */
  removed: Record<string, string>;
}

/**
 * Where act's copies of the workflows live, relative to the snapshot root. Inside `.git`, so git
 * never tracks or reports them and nothing in the job mistakes them for the project's workflows.
 * act still accepts the path: a local reusable workflow only has to be a `./` path in the repo.
 */
export const ACT_WORKFLOWS_DIR = '.git/gridstator/workflows';

/** Extracts `owner/repo` from a GitHub-style remote URL (https, ssh or scp form). */
export function repoSlug(remoteUrl: string | undefined): string | undefined {
  if (!remoteUrl) return undefined;
  const m = /[/:]([^/:]+)\/([^/]+?)(?:\.git)?\/?$/.exec(remoteUrl.trim());
  return m ? `${m[1]}/${m[2]}` : undefined;
}

const CHECKOUT = /^actions\/checkout@/;
const GITHUB_REPOSITORY_EXPR = /^\$\{\{\s*github\.repository\s*\}\}$/;
const WORKFLOW_FILE = /\.ya?ml$/;

function isSameRepository(value: string, slug: string | undefined): boolean {
  const v = value.trim();
  return (
    GITHUB_REPOSITORY_EXPR.test(v) || (slug !== undefined && v.toLowerCase() === slug.toLowerCase())
  );
}

const text = (node: unknown): string | undefined => {
  if (!isScalar(node)) return undefined;
  const v = node.value;
  return typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean'
    ? String(v)
    : undefined;
};

/** Removes `ref`/`repository` from the document's same-repository checkouts, in place. */
function rewriteCheckouts(doc: Document, file: string, slug?: string): CheckoutRewrite[] {
  const jobs = doc.get('jobs');
  if (!isMap(jobs)) return [];
  const rewrites: CheckoutRewrite[] = [];
  for (const jobPair of jobs.items) {
    const job = jobPair.value;
    const steps = isMap(job) ? job.get('steps') : undefined;
    if (!isSeq(steps)) continue;
    steps.items.forEach((step, index) => {
      if (!isMap(step)) return;
      const uses = text(step.get('uses', true));
      if (!uses || !CHECKOUT.test(uses.trim())) return;
      const withMap = step.get('with');
      if (!isMap(withMap)) return;
      const repository = text(withMap.get('repository', true));
      if (repository !== undefined && !isSameRepository(repository, slug)) return;
      const removed: Record<string, string> = {};
      for (const key of ['ref', 'repository']) {
        if (!withMap.has(key)) continue;
        removed[key] = text(withMap.get(key, true)) ?? '';
        withMap.delete(key);
      }
      if (Object.keys(removed).length === 0) return;
      if (withMap.items.length === 0) step.delete('with');
      rewrites.push({
        file,
        job: text(jobPair.key) ?? '?',
        step: text(step.get('name', true)) ?? `#${index + 1} (${uses})`,
        removed,
      });
    });
  }
  return rewrites;
}

/**
 * Points `jobs.<id>.uses: ./.github/workflows/<name>` at act's copy of `<name>`, in place, so a
 * called workflow gets its rewrite too. Returns whether anything changed.
 */
function redirectLocalCalls(doc: Document, copied: ReadonlySet<string>): boolean {
  const jobs = doc.get('jobs');
  if (!isMap(jobs)) return false;
  let changed = false;
  for (const jobPair of jobs.items) {
    const job = jobPair.value;
    if (!isMap(job)) continue;
    const uses = text(job.get('uses', true))?.trim();
    if (!uses?.startsWith('./')) continue;
    const rel = posix.normalize(uses);
    if (posix.dirname(rel) !== '.github/workflows' || !copied.has(posix.basename(rel))) continue;
    job.set('uses', `./${ACT_WORKFLOWS_DIR}/${posix.basename(rel)}`);
    changed = true;
  }
  return changed;
}

/** Rewrites one workflow's same-repository checkouts; returns the original text when unchanged. */
export function rewriteLocalCheckouts(
  source: string,
  file: string,
  slug?: string,
): { text: string; rewrites: CheckoutRewrite[] } {
  const doc = parseDocument(source);
  if (doc.errors.length > 0) return { text: source, rewrites: [] };
  const rewrites = rewriteCheckouts(doc, file, slug);
  return rewrites.length > 0
    ? { text: doc.toString({ lineWidth: 0 }), rewrites }
    : { text: source, rewrites };
}

export interface ActWorkflows {
  rewrites: CheckoutRewrite[];
  /**
   * Snapshot-relative workflow path → act's copy of it (also snapshot-relative), e.g.
   * `.github/workflows/ci.yml` → `.git/gridstator/workflows/ci.yml`. Empty when no workflow needed
   * a rewrite, so act reads the originals.
   */
  copies: Map<string, string>;
}

/**
 * Writes act's copies of every workflow under `<root>/.github/workflows` into
 * `<root>/{@link ACT_WORKFLOWS_DIR}`, with same-repository checkouts rewritten and local reusable
 * workflow calls pointed at the copies. Writes nothing when no checkout needs rewriting. The
 * originals are never modified.
 */
export async function writeActWorkflows(root: string, slug?: string): Promise<ActWorkflows> {
  const dir = join(root, '.github', 'workflows');
  let names: string[];
  try {
    names = (await readdir(dir)).filter((n) => WORKFLOW_FILE.test(n)).sort();
  } catch {
    return { rewrites: [], copies: new Map() };
  }
  const workflows: { name: string; source: string; doc?: Document; rewritten: boolean }[] = [];
  const rewrites: CheckoutRewrite[] = [];
  for (const name of names) {
    let source: string;
    try {
      source = await readFile(join(dir, name), 'utf8');
    } catch {
      continue; // e.g. a directory named like a workflow
    }
    const doc = parseDocument(source);
    if (doc.errors.length > 0) {
      // Copied verbatim, so act reports the same error it would for the original.
      workflows.push({ name, source, rewritten: false });
      continue;
    }
    const found = rewriteCheckouts(doc, `.github/workflows/${name}`, slug);
    rewrites.push(...found);
    workflows.push({ name, source, doc, rewritten: found.length > 0 });
  }
  if (rewrites.length === 0) return { rewrites, copies: new Map() };

  const copied = new Set(workflows.map((w) => w.name));
  const out = join(root, ...ACT_WORKFLOWS_DIR.split('/'));
  await mkdir(out, { recursive: true });
  const copies = new Map<string, string>();
  for (const w of workflows) {
    const redirected = w.doc ? redirectLocalCalls(w.doc, copied) : false;
    await writeFile(
      join(out, w.name),
      w.doc && (w.rewritten || redirected) ? w.doc.toString({ lineWidth: 0 }) : w.source,
    );
    copies.set(`.github/workflows/${w.name}`, `${ACT_WORKFLOWS_DIR}/${w.name}`);
  }
  return { rewrites, copies };
}

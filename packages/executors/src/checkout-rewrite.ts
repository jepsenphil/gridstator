import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { isMap, isScalar, isSeq, parseDocument } from 'yaml';

/**
 * act skips `actions/checkout` (and copies the snapshot into the job container instead) only when
 * the step's raw `with.ref` / `with.repository` text equals `github.ref` / `github.repository`.
 * It never evaluates expressions, so a step like `ref: ${{ inputs.source_ref || github.sha }}`
 * runs the real checkout: it needs a token, fetches from GitHub, and never sees the snapshot.
 *
 * Gridstator runs exactly the submitted working tree, so every checkout of the workflow's own
 * repository is pointed at the snapshot by removing `ref` (and a same-repository `repository`)
 * from the private snapshot copy. The developer's files are never touched. Checkouts of other
 * repositories are left alone.
 */
export interface CheckoutRewrite {
  /** Workflow path relative to the snapshot root. */
  file: string;
  job: string;
  step: string;
  /** Inputs removed from the step, with their original text. */
  removed: Record<string, string>;
}

/** Extracts `owner/repo` from a GitHub-style remote URL (https, ssh or scp form). */
export function repoSlug(remoteUrl: string | undefined): string | undefined {
  if (!remoteUrl) return undefined;
  const m = /[/:]([^/:]+)\/([^/]+?)(?:\.git)?\/?$/.exec(remoteUrl.trim());
  return m ? `${m[1]}/${m[2]}` : undefined;
}

const CHECKOUT = /^actions\/checkout@/;
const GITHUB_REPOSITORY_EXPR = /^\$\{\{\s*github\.repository\s*\}\}$/;

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

/** Rewrites one workflow's same-repository checkouts; returns the original text when unchanged. */
export function rewriteLocalCheckouts(
  source: string,
  file: string,
  slug?: string,
): { text: string; rewrites: CheckoutRewrite[] } {
  const doc = parseDocument(source);
  if (doc.errors.length > 0) return { text: source, rewrites: [] };
  const jobs = doc.get('jobs');
  if (!isMap(jobs)) return { text: source, rewrites: [] };
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
  return rewrites.length > 0
    ? { text: doc.toString({ lineWidth: 0 }), rewrites }
    : { text: source, rewrites };
}

/**
 * Applies {@link rewriteLocalCheckouts} to every workflow under `<root>/.github/workflows`, so
 * local reusable workflows (`uses: ./.github/workflows/x.yml`) are covered too.
 */
export async function rewriteSnapshotCheckouts(
  root: string,
  slug?: string,
): Promise<CheckoutRewrite[]> {
  const dir = join(root, '.github', 'workflows');
  let names: string[];
  try {
    names = (await readdir(dir)).filter((n) => /\.ya?ml$/.test(n)).sort();
  } catch {
    return [];
  }
  const all: CheckoutRewrite[] = [];
  for (const name of names) {
    const path = join(dir, name);
    const source = await readFile(path, 'utf8');
    const { text: out, rewrites } = rewriteLocalCheckouts(
      source,
      `.github/workflows/${name}`,
      slug,
    );
    if (rewrites.length === 0) continue;
    await writeFile(path, out);
    all.push(...rewrites);
  }
  return all;
}

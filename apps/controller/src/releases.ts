import { Readable } from 'node:stream';
import { z } from 'zod';
import { GridstatorError, GRIDSTATOR_VERSION, errorMessage } from '@gridstator/shared';
import type { PublishBuildResponse, ReleaseView } from '@gridstator/protocol';
import type { SelfUpdater } from './self-update.js';
import type { Fetcher } from './toolchain.js';

const REPOSITORY = 'jepsenphil/gridstator';
const API = `https://api.github.com/repos/${REPOSITORY}/releases`;
const WEB = `https://github.com/${REPOSITORY}`;
const TAG = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/;
const CACHE_MS = 5 * 60_000;

const GithubReleaseSchema = z.object({
  tag_name: z.string(),
  name: z.string().nullable(),
  body: z.string().nullable(),
  published_at: z.string().nullable(),
  draft: z.boolean(),
  prerelease: z.boolean(),
  assets: z.array(z.object({ name: z.string() })),
});

const releaseFetcher: Fetcher = async (url) => {
  const response = await fetch(url, {
    headers: {
      accept: 'application/vnd.github+json',
      'user-agent': `gridstator/${GRIDSTATOR_VERSION}`,
      'x-github-api-version': '2022-11-28',
    },
    redirect: 'follow',
    signal: AbortSignal.timeout(10 * 60_000),
  });
  if (!response.ok || !response.body) throw new Error(`GitHub returned HTTP ${response.status}`);
  return Readable.fromWeb(response.body);
};

/** Official release discovery and checksum-verified installation through the existing updater. */
export class ReleaseService {
  private readonly fetcher: Fetcher;
  private cache: { at: number; value: ReleaseView[] } | undefined;
  private loading: Promise<ReleaseView[]> | undefined;
  private installing = false;

  constructor(
    private readonly updater: Pick<SelfUpdater, 'publish' | 'capability'>,
    fetcher?: Fetcher,
  ) {
    this.fetcher = fetcher ?? releaseFetcher;
  }

  async list(includePrereleases = false): Promise<ReleaseView[]> {
    let releases: ReleaseView[];
    if (this.cache && Date.now() - this.cache.at < CACHE_MS) releases = this.cache.value;
    else {
      this.loading ??= this.load().finally(() => {
        this.loading = undefined;
      });
      releases = await this.loading;
    }
    return releases.filter((release) => includePrereleases || !release.prerelease);
  }

  private async load(): Promise<ReleaseView[]> {
    try {
      // Bounded to the most recent 100 published releases; installation also resolves exact tags.
      const raw: unknown = JSON.parse(
        await readText(await this.fetcher(`${API}?per_page=100`), 4 * 1024 * 1024),
      );
      const releases = z.array(GithubReleaseSchema).parse(raw);
      const value = releases
        .map(asRelease)
        .filter((release): release is ReleaseView => release !== null)
        .sort((a, b) => b.publishedAt - a.publishedAt);
      this.cache = { at: Date.now(), value };
      return value;
    } catch (err) {
      throw new GridstatorError(
        'UNAVAILABLE',
        `Could not check Gridstator releases: ${errorMessage(err)}`,
        {
          hint: 'Check the controller’s internet connection and try again. Local package upload remains available.',
          cause: err,
        },
      );
    }
  }

  async install(tag: string): Promise<PublishBuildResponse> {
    if (!TAG.test(tag))
      throw new GridstatorError('BAD_REQUEST', 'Choose a published Gridstator release tag.');
    const capability = this.updater.capability();
    if (!capability.available)
      throw new GridstatorError(
        'CONFLICT',
        capability.reason ?? 'The controller cannot update itself.',
      );
    if (this.installing)
      throw new GridstatorError('CONFLICT', 'Another release is being installed.');
    this.installing = true;
    try {
      let release: ReleaseView | null;
      try {
        const raw: unknown = JSON.parse(
          await readText(await this.fetcher(`${API}/tags/${encodeURIComponent(tag)}`), 1024 * 1024),
        );
        release = asRelease(GithubReleaseSchema.parse(raw));
      } catch (err) {
        throw new GridstatorError(
          'UNAVAILABLE',
          `Could not read release ${tag}: ${errorMessage(err)}`,
          { cause: err },
        );
      }
      if (!release || release.tag !== tag)
        throw new GridstatorError(
          'NOT_FOUND',
          `Release ${tag} has no Gridstator package with published checksums.`,
        );
      const base = `${WEB}/releases/download/${encodeURIComponent(tag)}`;
      let checksums: string;
      try {
        checksums = await readText(await this.fetcher(`${base}/SHA256SUMS`), 1024 * 1024);
      } catch (err) {
        throw new GridstatorError(
          'UNAVAILABLE',
          `Could not download release checksums: ${errorMessage(err)}`,
          { cause: err },
        );
      }
      const sha256 = checksumFor(checksums, release.packageName);
      if (!sha256)
        throw new GridstatorError(
          'BAD_REQUEST',
          `The published checksums do not contain ${release.packageName}.`,
        );
      let body: Readable;
      try {
        body = await this.fetcher(`${base}/${encodeURIComponent(release.packageName)}`);
      } catch (err) {
        throw new GridstatorError(
          'UNAVAILABLE',
          `Could not download release ${tag}: ${errorMessage(err)}`,
          { cause: err },
        );
      }
      // The updater hashes the complete stream and checks it before unpacking or switching builds.
      return await this.updater.publish(body, sha256);
    } finally {
      this.installing = false;
    }
  }
}

function asRelease(raw: z.infer<typeof GithubReleaseSchema>): ReleaseView | null {
  const version = TAG.exec(raw.tag_name)?.[1];
  const publishedAt = raw.published_at ? Date.parse(raw.published_at) : NaN;
  if (!version || raw.draft || !Number.isFinite(publishedAt)) return null;
  const packageName = `gridstator-${version}.tgz`;
  if (
    !raw.assets.some((asset) => asset.name === packageName) ||
    !raw.assets.some((asset) => asset.name === 'SHA256SUMS')
  )
    return null;
  return {
    tag: raw.tag_name,
    version,
    name: raw.name ?? raw.tag_name,
    url: `${WEB}/releases/tag/${encodeURIComponent(raw.tag_name)}`,
    publishedAt,
    prerelease: raw.prerelease || version.includes('-'),
    notes: raw.body ?? '',
    packageName,
  };
}

function checksumFor(text: string, name: string): string | undefined {
  const matches = text
    .split('\n')
    .map((line) => /^([0-9a-f]{64})\s+\*?(\S+)$/i.exec(line.trim()))
    .filter((match) => match?.[2] === name);
  // Ambiguous manifests are not a trustworthy source of the expected package hash.
  return matches.length === 1 ? matches[0]?.[1]?.toLowerCase() : undefined;
}

async function readText(body: Readable, maxBytes: number): Promise<string> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of body) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as Uint8Array);
    size += bytes.length;
    if (size > maxBytes) throw new Error('GitHub response exceeded the size limit');
    chunks.push(bytes);
  }
  return Buffer.concat(chunks).toString('utf8');
}

const BYTE_UNITS: Record<string, number> = {
  b: 1,
  k: 1024,
  kb: 1024,
  ki: 1024,
  kib: 1024,
  m: 1024 ** 2,
  mb: 1024 ** 2,
  mi: 1024 ** 2,
  mib: 1024 ** 2,
  g: 1024 ** 3,
  gb: 1024 ** 3,
  gi: 1024 ** 3,
  gib: 1024 ** 3,
  t: 1024 ** 4,
  tb: 1024 ** 4,
  ti: 1024 ** 4,
  tib: 1024 ** 4,
};

/** Parses sizes such as `512m`, `8g`, `1.5GiB` or a plain byte count. Units are binary. */
export function parseBytes(input: string): number {
  const match = /^\s*(\d+(?:\.\d+)?)\s*([a-zA-Z]*)\s*$/.exec(input);
  if (!match) throw new Error(`Invalid size "${input}" (expected e.g. 512m, 8g)`);
  const value = Number(match[1]);
  const unit = (match[2] ?? '').toLowerCase() || 'b';
  const mult = BYTE_UNITS[unit];
  if (mult === undefined) throw new Error(`Invalid size unit "${match[2]}" in "${input}"`);
  return Math.round(value * mult);
}

/** Parses a size and returns whole mebibytes. */
export function parseMegabytes(input: string): number {
  if (/^\s*\d+\s*$/.test(input)) return Number(input);
  return Math.round(parseBytes(input) / 1024 ** 2);
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return '-';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let v = bytes;
  let i = 0;
  while (Math.abs(v) >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 || i === 0 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

const DURATION_UNITS: Record<string, number> = {
  ms: 1,
  s: 1000,
  m: 60_000,
  h: 3_600_000,
  d: 86_400_000,
};

/** Parses durations such as `30s`, `15m`, `24h`, `7d`, `1h30m`. Returns milliseconds. */
export function parseDuration(input: string): number {
  const re = /(\d+(?:\.\d+)?)(ms|s|m|h|d)/g;
  const trimmed = input.trim();
  let total = 0;
  let consumed = 0;
  for (const m of trimmed.matchAll(re)) {
    total += Number(m[1]) * (DURATION_UNITS[m[2] ?? ''] ?? 0);
    consumed += m[0].length;
  }
  if (consumed === 0 || consumed !== trimmed.length) {
    throw new Error(`Invalid duration "${input}" (expected e.g. 30s, 15m, 24h)`);
  }
  return Math.round(total);
}

export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '-';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${(ms / 1000).toFixed(s < 10 ? 1 : 0)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, '0')}s`;
  const h = Math.floor(m / 60);
  return `${h}h${String(m % 60).padStart(2, '0')}m`;
}

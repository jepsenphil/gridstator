import type { Command } from 'commander';
import { GridstatorError, parseMegabytes } from '@gridstator/shared';
import { LabelSchema, type NodeSettingsPatch } from '@gridstator/protocol';

export interface SettingsFlags {
  maxConcurrency?: string;
  reserveCpus?: string;
  reserveMemory?: string;
  labels?: string;
}

export function addSettingsFlags(cmd: Command): Command {
  return cmd
    .option('--max-concurrency <n>', 'maximum number of jobs this node runs at once')
    .option('--reserve-cpus <n>', 'CPUs held back for the host (never given to jobs)')
    .option('--reserve-memory <size>', 'memory held back for the host, e.g. 8g')
    .option('--labels <list>', 'comma-separated node labels, e.g. fast,ssd');
}

export function parseLabels(list: string): string[] {
  const labels = list
    .split(',')
    .map((l) => l.trim())
    .filter(Boolean);
  for (const l of labels) {
    if (!LabelSchema.safeParse(l).success)
      throw new GridstatorError('BAD_REQUEST', `Invalid label "${l}"`);
  }
  return labels;
}

function positiveNumber(value: string, flag: string, integer: boolean): number {
  const n = Number(value);
  if (!Number.isFinite(n) || n < 0 || (integer && !Number.isInteger(n))) {
    throw new GridstatorError('BAD_REQUEST', `${flag} must be a ${integer ? 'whole ' : ''}number`);
  }
  return n;
}

export function settingsFromFlags(flags: SettingsFlags): NodeSettingsPatch | undefined {
  const patch: NodeSettingsPatch = {};
  if (flags.maxConcurrency !== undefined)
    patch.maxConcurrency = positiveNumber(flags.maxConcurrency, '--max-concurrency', true);
  if (flags.reserveCpus !== undefined || flags.reserveMemory !== undefined) {
    patch.reserved = {
      ...(flags.reserveCpus !== undefined
        ? { cpus: positiveNumber(flags.reserveCpus, '--reserve-cpus', false) }
        : {}),
      ...(flags.reserveMemory !== undefined
        ? { memoryMb: parseMegabytes(flags.reserveMemory) }
        : {}),
    };
  }
  if (flags.labels !== undefined) patch.labels = parseLabels(flags.labels);
  return Object.keys(patch).length > 0 ? patch : undefined;
}

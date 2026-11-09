import { readFile } from 'node:fs/promises';
import { z } from 'zod';
import {
  GridstatorError,
  defaultClientConfigPath,
  readJsonFile,
  writeJsonFile,
} from '@gridstator/shared';
import { GridstatorClient } from './client.js';

export const ClientConfigSchema = z.object({
  version: z.literal(1),
  url: z.string(),
  caPem: z.string(),
  caFingerprint: z.string(),
  token: z.string(),
  tokenName: z.string().optional(),
});
export type ClientConfig = z.infer<typeof ClientConfigSchema>;

export async function readClientConfig(
  path = defaultClientConfigPath(),
): Promise<ClientConfig | undefined> {
  const raw = await readJsonFile(path);
  if (raw === undefined) return undefined;
  const parsed = ClientConfigSchema.safeParse(raw);
  if (!parsed.success) {
    throw new GridstatorError('CONFIG', `Client configuration ${path} is invalid`, {
      hint: 'Run `gridstator login` again to recreate it.',
    });
  }
  return parsed.data;
}

export async function writeClientConfig(
  config: ClientConfig,
  path = defaultClientConfigPath(),
): Promise<void> {
  await writeJsonFile(path, config, 0o600);
}

/**
 * Resolves connection settings: environment variables (GRIDSTATOR_URL, GRIDSTATOR_TOKEN,
 * GRIDSTATOR_CA_FILE) override the saved client configuration field by field.
 */
export async function resolveConnection(
  path = defaultClientConfigPath(),
): Promise<{ url: string; ca: string; token: string; source: string }> {
  const saved = await readClientConfig(path);
  const envCaFile = process.env['GRIDSTATOR_CA_FILE'];
  const url = process.env['GRIDSTATOR_URL'] ?? saved?.url;
  const token = process.env['GRIDSTATOR_TOKEN'] ?? saved?.token;
  const ca = envCaFile ? await readFile(envCaFile, 'utf8') : saved?.caPem;
  if (!url || !token || !ca) {
    throw new GridstatorError('CONFIG', 'Not connected to a Gridstator controller', {
      hint: 'Run `gridstator login <url> --fingerprint <sha256:...> --token <token>` (printed by `gridstator controller init`).',
    });
  }
  return { url, ca, token, source: saved ? path : 'environment' };
}

export async function connect(path = defaultClientConfigPath()): Promise<GridstatorClient> {
  const conn = await resolveConnection(path);
  return new GridstatorClient(conn);
}

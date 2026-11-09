import { join } from 'node:path';
import { z } from 'zod';
import { GridstatorError, readJsonFile, writeJsonFile } from '@gridstator/shared';

export const AgentStateSchema = z.object({
  version: z.literal(1),
  controllerUrl: z.string(),
  caPem: z.string(),
  caFingerprint: z.string(),
  nodeId: z.string(),
  name: z.string(),
  credential: z.string(),
});
export type AgentState = z.infer<typeof AgentStateSchema>;

export function agentStatePath(dataDir: string): string {
  return join(dataDir, 'agent.json');
}

export async function readAgentState(dataDir: string): Promise<AgentState | undefined> {
  const raw = await readJsonFile(agentStatePath(dataDir));
  if (raw === undefined) return undefined;
  const parsed = AgentStateSchema.safeParse(raw);
  if (!parsed.success) {
    throw new GridstatorError('CONFIG', `Agent state ${agentStatePath(dataDir)} is invalid`, {
      hint: 'Re-join the cluster with `gridstator agent join --force`.',
    });
  }
  return parsed.data;
}

export async function writeAgentState(dataDir: string, state: AgentState): Promise<void> {
  await writeJsonFile(agentStatePath(dataDir), state, 0o600);
}

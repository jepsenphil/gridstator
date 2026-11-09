import { existsSync } from 'node:fs';
import { hostname } from 'node:os';
import { GridstatorError, certificateFingerprint, ensureDir } from '@gridstator/shared';
import type { NodeSettingsPatch } from '@gridstator/protocol';
import { GridstatorClient, fetchPinnedCa, normalizeControllerUrl } from '@gridstator/client';
import { DockerCli } from '@gridstator/executors';
import { agentStatePath, writeAgentState, type AgentState } from './state.js';
import { collectNodeInfo } from './system.js';

export function defaultNodeName(): string {
  return (
    hostname()
      .split('.')[0]!
      .replace(/[^A-Za-z0-9._-]/g, '-')
      .replace(/^[^A-Za-z0-9]+/, '') || 'node'
  );
}

/** Registers this machine with a controller using a join token and saves the agent credential. */
export async function joinCluster(options: {
  url: string;
  joinToken: string;
  fingerprint: string;
  dataDir: string;
  name?: string;
  settings?: NodeSettingsPatch;
  force?: boolean;
  docker?: DockerCli;
}): Promise<AgentState> {
  if (existsSync(agentStatePath(options.dataDir)) && !options.force) {
    throw new GridstatorError(
      'CONFLICT',
      `This machine already joined a cluster (${agentStatePath(options.dataDir)})`,
      {
        hint: 'Pass --force to join again (remove the old node on the controller with `gridstator nodes remove`).',
      },
    );
  }
  const url = normalizeControllerUrl(options.url);
  const ca = await fetchPinnedCa(url, options.fingerprint);
  const client = new GridstatorClient({ url, ca, token: options.joinToken });
  try {
    const info = await collectNodeInfo(options.docker ?? new DockerCli());
    const res = await client.registerAgent(options.joinToken, {
      name: options.name ?? defaultNodeName(),
      info,
      ...(options.settings ? { settings: options.settings } : {}),
    });
    const state: AgentState = {
      version: 1,
      controllerUrl: url,
      caPem: ca,
      caFingerprint: certificateFingerprint(ca),
      nodeId: res.nodeId,
      name: res.name,
      credential: res.credential,
    };
    await ensureDir(options.dataDir);
    await writeAgentState(options.dataDir, state);
    return state;
  } finally {
    client.close();
  }
}

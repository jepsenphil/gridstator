import { GridstatorClient, normalizeControllerUrl } from '@gridstator/client';
import { writeAgentState, type AgentState } from './state.js';

/** Retarget the existing client so active runners and the updater retain their credentials. */
export async function relocateController(
  dataDir: string,
  state: AgentState,
  client: GridstatorClient,
  endpoint: string,
): Promise<boolean> {
  const url = normalizeControllerUrl(endpoint);
  if (url === client.url) return false;
  const probe = new GridstatorClient({ url, ca: state.caPem, token: state.credential });
  try {
    await probe.health();
  } finally {
    probe.close();
  }
  const next: AgentState = {
    ...state,
    controllerUrl: url,
    controllerFallbackUrls: [
      ...new Set([state.controllerUrl, ...(state.controllerFallbackUrls ?? [])]),
    ]
      .filter((previous) => previous !== url)
      .slice(0, 8),
  };
  // Commit to disk first: a process restart must use the address that was verified.
  await writeAgentState(dataDir, next);
  Object.assign(state, next);
  client.setControllerUrl(url);
  return true;
}

import { GridstatorClient, normalizeControllerUrl } from '@gridstator/client';
import { updateAgentState, type AgentState } from './state.js';

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
  // Construct the update inside the state queue so a concurrent worker rename is retained.
  await updateAgentState(dataDir, state, (current) => ({
    ...current,
    controllerUrl: url,
    controllerFallbackUrls: [
      ...new Set([current.controllerUrl, ...(current.controllerFallbackUrls ?? [])]),
    ]
      .filter((previous) => previous !== url)
      .slice(0, 8),
  }));
  client.setControllerUrl(url);
  return true;
}

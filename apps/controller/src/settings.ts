import {
  ClusterSettingsSchema,
  DEFAULT_CLUSTER_SETTINGS,
  type ClusterSettings,
} from '@gridstator/protocol';
import { GridstatorError } from '@gridstator/shared';
import type { Store } from './db.js';

const KEY = 'cluster_settings';

export class SettingsService {
  private readonly store: Store;

  constructor(store: Store) {
    this.store = store;
  }

  get(): ClusterSettings {
    const raw = this.store.getMeta(KEY);
    const parsed = raw
      ? ClusterSettingsSchema.safeParse({ ...DEFAULT_CLUSTER_SETTINGS, ...JSON.parse(raw) })
      : undefined;
    return parsed?.success ? parsed.data : { ...DEFAULT_CLUSTER_SETTINGS };
  }

  update(patch: Partial<ClusterSettings>): ClusterSettings {
    const next = ClusterSettingsSchema.parse({ ...this.get(), ...patch });
    if (next.autoUpdate === 'window' && next.updateWindow === null) {
      throw new GridstatorError('BAD_REQUEST', 'Auto-update mode "window" needs an update window', {
        hint: 'Set one, e.g. `gridstator agents set --auto-update window --window 02:00-05:00`.',
      });
    }
    this.store.setMeta(KEY, JSON.stringify(next));
    return next;
  }
}

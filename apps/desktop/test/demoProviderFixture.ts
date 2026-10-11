import type { ModelControl } from '../src/main/hostBridge.js';
import type { ProviderSettings } from '../src/main/providerSettings.js';

/** Explicit no-credential demo authority for standalone IPC and Stop fixtures. */
export const demoModels = { authorization: { phase: 'not_configured', route: null, credential_kind: null,
  storage: null, authorized_purposes: [], can_authorize: false, can_save: false,
  planning_available: true, saved: [], reason: null } } as unknown as ModelControl;
export const demoProviders = { snapshot: { providers: [], connections: [], selected: null,
  busy: false, can_configure: false, keychain_available: false, reason: null } } as unknown as ProviderSettings;

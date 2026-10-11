import type { CapabilityManifest } from '@computer-use/desktop-api';

/** Native observations stay readonly until a separately authorized action exists. */
export function nativeUnavailableDomains(): CapabilityManifest['domains'] {
  return [
    { domain: 'foreground_native', state: 'unavailable', actions: [] },
    { domain: 'background_ax', state: 'unavailable', actions: [] },
    { domain: 'headless_browser', state: 'unavailable', actions: [] },
    { domain: 'app_integration', state: 'unavailable', actions: [] },
    { domain: 'simulated', state: 'unavailable', actions: [] },
  ];
}

/** The simulator advertises only the action family it actually implements. */
export function simulatedOnlyDomains(): CapabilityManifest['domains'] {
  return [
    { domain: 'foreground_native', state: 'unavailable', actions: [] },
    { domain: 'background_ax', state: 'unavailable', actions: [] },
    { domain: 'headless_browser', state: 'unavailable', actions: [] },
    { domain: 'app_integration', state: 'unavailable', actions: [] },
    { domain: 'simulated', state: 'available', actions: ['simulated_action'] },
  ];
}

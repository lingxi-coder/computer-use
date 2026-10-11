import type { SupervisorOptions } from './supervisor.js';

function freeze(value: unknown): void {
  if (!value || typeof value !== 'object' || Object.isFrozen(value)) return;
  for (const child of Object.values(value)) freeze(child);
  Object.freeze(value);
}

/** Main-process diagnostics carry copies and cannot change resource authority. */
function emit(name: string, value: unknown): void {
  if (process.listenerCount(name) === 0) return;
  const copy = structuredClone(value);
  freeze(copy);
  for (const listener of process.rawListeners(name)) {
    try { listener.call(process, copy); } catch { /* Observers cannot interrupt execution. */ }
  }
}

export const browserDiagnostics: Pick<SupervisorOptions,
  'observeBrowserSourceOwner' | 'observeBrowserSnapshot' | 'observeBrowserExecution'> = {
  observeBrowserSourceOwner: (value) => emit('computer-use:browser-owner', value),
  observeBrowserSnapshot: (value) => emit('computer-use:browser-snapshot', value),
  observeBrowserExecution: (value) => emit('computer-use:browser-execution', value),
};

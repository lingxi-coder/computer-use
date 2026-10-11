// Adapted from LingXi Electron `apps/electron/src/renderer/bridge/providerCredentials.ts`.
import type { ConfigureProviderRequest, ModelPurpose } from '@computer-use/desktop-api';

/** All interactive provider credentials use Main-owned macOS Keychain storage. */
export function keychainConfigureProviderRequest(
  providerId: string,
  purposes: readonly ModelPurpose[],
  options: { apiKey?: string; reauthenticate?: boolean } = {},
): ConfigureProviderRequest {
  return {
    provider_id: providerId,
    storage: 'keychain',
    purposes: [...purposes],
    ...(options.apiKey !== undefined ? { api_key: options.apiKey } : {}),
    ...(options.reauthenticate === true ? { reauthenticate: true } : {}),
  };
}

/** Async provider work may finish after its detail view has been left or closed. */
export function isCurrentCredentialTransaction(
  mounted: boolean,
  transactionGeneration: number,
  currentGeneration: number,
): boolean {
  return mounted && transactionGeneration === currentGeneration;
}

/** Persist a trimmed API key and clear the renderer's copy on every outcome. */
export async function persistProviderCredentialInput(
  input: string,
  persist: (credential: string) => Promise<unknown>,
  clearSecret: () => void,
): Promise<boolean> {
  const credential = input.trim();
  if (!credential) return false;
  try {
    await persist(credential);
    return true;
  } finally {
    clearSecret();
  }
}

/** Apply completion callbacks only while the caller still owns the transaction. */
export async function runCurrentProviderTransaction<T>(
  isCurrent: () => boolean,
  operation: () => Promise<T>,
  onSuccess: (result: T) => void,
  onFailure: (error: unknown) => void,
  onFinally: () => void,
): Promise<void> {
  let result: T;
  try {
    result = await operation();
  } catch (error) {
    if (isCurrent()) onFailure(error);
    if (isCurrent()) onFinally();
    return;
  }
  if (isCurrent()) onSuccess(result);
  if (isCurrent()) onFinally();
}

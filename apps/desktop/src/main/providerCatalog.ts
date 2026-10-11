import type { ModelProviderDefinition, ProviderModel } from '@computer-use/desktop-api';
import { validateModelRoute, type ModelCredential } from './modelBootstrap.js';
import { requestPrivateHelper } from './openAiLogin.js';

const MAX_CATALOG_BYTES = 2 * 1024 * 1024;
function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function exact(value: Record<string, unknown>, keys: string[]): boolean {
  return Object.keys(value).sort().join(',') === keys.sort().join(',');
}
function text(value: unknown, limit = 256): value is string {
  return typeof value === 'string' && value.trim().length > 0 && Buffer.byteLength(value) <= limit && !/\p{Cc}/u.test(value);
}
function failure(): Error { return new Error('The provider model catalog is unavailable.'); }

export function providerModels(value: unknown): ProviderModel[] {
  if (!Array.isArray(value) || value.length > 4096) throw failure();
  const seen = new Set<string>();
  return value.map((row: unknown) => {
    if (!record(row) || !exact(row, ['id', 'label', 'selectable']) || !text(row.id)
      || !text(row.label) || typeof row.selectable !== 'boolean' || seen.has(row.id)) throw failure();
    seen.add(row.id);
    return { id: row.id, label: row.label, selectable: row.selectable };
  });
}

export function providerCatalog(value: unknown): ModelProviderDefinition[] {
  if (!record(value) || !exact(value, ['providers']) || !Array.isArray(value.providers) || value.providers.length > 128) throw failure();
  const seen = new Set<string>();
  return value.providers.map((row: unknown) => {
    if (!record(row) || !exact(row, ['id', 'label', 'description', 'route', 'auth', 'models', 'directory_supported'])
      || !text(row.id, 128) || !/^[a-z0-9][a-z0-9._-]*$/.test(row.id) || seen.has(row.id)
      || !text(row.label) || typeof row.description !== 'string' || Buffer.byteLength(row.description) > 2048 || /\p{Cc}/u.test(row.description)
      || !record(row.route) || !exact(row.route, ['provider_id', 'protocol', 'base_url', 'max_output_tokens', 'preset_id'])
      || row.route.preset_id !== row.id || (row.auth !== 'api_key' && row.auth !== 'openai_oauth')
      || typeof row.directory_supported !== 'boolean') throw failure();
    const checked = validateModelRoute({ ...row.route, model: 'catalog-validation' });
    if (row.auth === 'openai_oauth' && (row.id !== 'openai-chatgpt' || checked.base_url !== 'https://chatgpt.com/backend-api/codex'
      || checked.protocol !== 'open_ai_responses' || checked.max_output_tokens !== 2048)) throw failure();
    seen.add(row.id);
    const { model: _model, ...route } = checked;
    return { id: row.id, label: row.label, description: row.description, route, auth: row.auth,
      models: providerModels(row.models), directory_supported: row.directory_supported };
  });
}

/** Only this private helper pipe sees credentials. SDK owns all vendor requests. */
export function createProviderCatalog(helperPath: string) {
  const request = async (body: Record<string, unknown>, signal?: AbortSignal): Promise<unknown> => {
    const timeout = AbortSignal.timeout(35_000);
    const bounded = signal ? AbortSignal.any([signal, timeout]) : timeout;
    try { return await requestPrivateHelper(helperPath, body, bounded, undefined, MAX_CATALOG_BYTES); }
    catch { throw failure(); }
  };
  return {
    load: async (): Promise<ModelProviderDefinition[]> => providerCatalog(await request({ operation: 'catalog' })),
    refresh: async (provider: ModelProviderDefinition, credential: ModelCredential, signal: AbortSignal): Promise<ProviderModel[]> => {
      const response = await request({ operation: 'models', provider_id: provider.id, credential }, signal);
      if (!record(response) || !exact(response, ['models'])) throw failure();
      return providerModels(response.models);
    },
  };
}

import React from 'react';
import type { ModelAuthorizationStatus, ModelProvider, ProviderSettingsSnapshot, TaskModelSelection } from '@computer-use/desktop-api';

// Keep the classic JSX test loader aligned with the renderer's react-jsx build.
void React;

function providerIdentity(provider: ModelProvider): string {
  if (typeof provider === 'string') return provider;
  if ('open_ai_compatible' in provider) return `open_ai_compatible:${provider.open_ai_compatible.name}`;
  return `custom:${provider.custom.name}`;
}

export function taskModelSelectionReady(
  snapshot: ProviderSettingsSnapshot | null,
  authorization: ModelAuthorizationStatus | null,
): boolean {
  if (!snapshot || snapshot.busy) return false;
  if (snapshot.connections.length === 0) return snapshot.reason === null && authorization?.phase === 'not_configured'
    && authorization.route === null && authorization.credential_kind === null && authorization.planning_available;
  const selected = snapshot.selected;
  if (!selected) return false;
  const connection = snapshot.connections.find((item) => item.id === selected.connection_id);
  const model = connection?.models.find((item) => item.id === selected.model && item.selectable);
  const provider = connection && snapshot.providers.find((item) => item.id === connection.provider_id);
  const route = authorization?.route;
  return connection?.available === true
    && connection.purposes.includes('s2_planning')
    && model !== undefined
    && provider !== undefined
    && authorization?.phase === 'authorized'
    && authorization.planning_available
    && authorization.authorized_purposes.includes('s2_planning')
    && authorization.storage === connection.storage
    && authorization.credential_kind === (provider.auth === 'openai_oauth' ? 'openai_oauth' : 'api_key')
    && route !== null
    && route !== undefined
    && route.model === selected.model
    && providerIdentity(route.provider_id) === providerIdentity(provider.route.provider_id)
    && route.preset_id === provider.route.preset_id
    && route.protocol === provider.route.protocol
    && route.base_url === provider.route.base_url
    && route.max_output_tokens === provider.route.max_output_tokens;
}

export function TaskModelPicker({
  snapshot,
  authorization,
  demoAvailable,
  disabled,
  pending,
  error,
  loadError,
  onSelect,
}: {
  snapshot: ProviderSettingsSnapshot | null;
  authorization: ModelAuthorizationStatus | null;
  demoAvailable: boolean;
  disabled: boolean;
  pending: boolean;
  error: string | null;
  loadError: string | null;
  onSelect(selection: TaskModelSelection): void;
}) {
  const availableConnections = snapshot?.connections.filter((connection) => connection.available) ?? [];
  const connectionCounts = new Map<string, number>();
  for (const connection of availableConnections) {
    connectionCounts.set(connection.provider_id, (connectionCounts.get(connection.provider_id) ?? 0) + 1);
  }
  const seenConnections = new Map<string, number>();
  const groups = availableConnections.map((connection) => {
    const provider = snapshot?.providers.find((item) => item.id === connection.provider_id);
    const index = (seenConnections.get(connection.provider_id) ?? 0) + 1;
    seenConnections.set(connection.provider_id, index);
    return {
      connection,
      label: `${provider?.label ?? connection.provider_id}${(connectionCounts.get(connection.provider_id) ?? 0) > 1 ? ` · Connection ${index}` : ''}`,
      models: connection.models.filter((model) => model.selectable),
    };
  }).filter((group) => group.models.length > 0);

  const selected = snapshot?.selected ?? null;
  const selectedGroup = selected && groups.find((group) => group.connection.id === selected.connection_id
    && group.models.some((model) => model.id === selected.model));
  const selectedModel = selectedGroup?.models.find((model) => model.id === selected?.model);
  const currentLabel = selected && selectedGroup && selectedModel
    ? `${selectedGroup.label} · ${selectedModel.label}`
    : selected
      ? 'Selected model unavailable'
      : snapshot?.connections.length === 0 && demoAvailable
        ? 'Demo planning'
        : snapshot && snapshot.connections.length > 0 && availableConnections.length === 0
          ? 'No configured provider is available'
          : 'No task model selected';
  const selectValue = selected && selectedGroup && selectedModel
    ? JSON.stringify([selected.connection_id, selected.model])
    : '';

  const handleChange = (value: string): void => {
    const option = groups.flatMap((group) => group.models.map((model) => ({
      token: JSON.stringify([group.connection.id, model.id]),
      selection: { connection_id: group.connection.id, model: model.id },
    }))).find((item) => item.token === value);
    if (option) onSelect(option.selection);
  };

  return <section className="task-model-picker" aria-label="Task model selection">
    <div className="task-model-current">
      <span>Current task model</span>
      <strong>{currentLabel}</strong>
      {selected && selectedGroup && selectedModel ? <code>{selected.model}</code> : null}
    </div>
    <label className="task-model-select">Choose for new tasks
      <select aria-label="Task model" value={selectValue}
        disabled={disabled || pending || snapshot?.busy === true || groups.length === 0}
        onChange={(event) => handleChange(event.target.value)}>
        <option value="">{groups.length > 0 ? 'Choose a configured model' : 'No available provider models'}</option>
        {groups.map((group) => <optgroup key={group.connection.id} label={group.label}>
          {group.models.map((model) => <option key={`${group.connection.id}:${model.id}`}
            value={JSON.stringify([group.connection.id, model.id])}>
            {model.label} · {model.id}
          </option>)}
        </optgroup>)}
      </select>
    </label>
    {pending ? <p className="task-model-pending" role="status">Changing task model. New tasks are paused until selection finishes.</p> : null}
    {error ? <p className="task-model-error" role="alert">{error}</p> : null}
    {loadError ? <p className="task-model-error" role="alert">{loadError}</p> : null}
    {snapshot?.reason ? <p className="task-model-status" role="status">{snapshot.reason}</p> : null}
    {snapshot === null && !loadError ? <p className="task-model-status" role="status">Loading configured task models.</p> : null}
    {snapshot && snapshot.connections.length > 0 && !taskModelSelectionReady(snapshot, authorization)
      ? <p className="task-model-status" role="status">Choose an available provider model with S2 task-planning access before starting a task.</p>
      : null}
    {snapshot && snapshot.connections.length === 0 && demoAvailable
      ? <p className="task-model-status" role="status">Demo planning is available; no provider model is selected.</p>
      : null}
    {snapshot && snapshot.connections.length === 0 && !demoAvailable
      ? <p className="task-model-status" role="status">Configure a provider or enable the existing demo planner before starting a task.</p>
      : null}
    {snapshot && snapshot.connections.length > 0 && availableConnections.length === 0
      ? <p className="task-model-error" role="status">A provider connection is configured but unavailable. Reconnect it before starting a task.</p>
      : null}
  </section>;
}

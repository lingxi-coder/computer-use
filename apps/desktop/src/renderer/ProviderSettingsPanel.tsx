import React, { useEffect, useRef, useState, type FormEvent } from 'react';
import type {
  ConfigureProviderRequest,
  ModelPurpose,
  ModelProviderDefinition,
  ProviderConnection,
  ProviderModel,
  ProviderSettingsSnapshot,
} from '@computer-use/desktop-api';
import {
  isCurrentCredentialTransaction,
  keychainConfigureProviderRequest,
  persistProviderCredentialInput,
  runCurrentProviderTransaction,
} from './electron-settings/providerSettingsLogic.js';

// Layout adapted from LingXi Electron's
// `apps/electron/src/renderer/components/settings/pages/ProviderCredentials.tsx`
// and `apps/electron/src/renderer/components/settings/rows.tsx`, using only
// Computer Use's local provider API. The renderer bootstrap imports the scoped CSS.

// Keep the classic JSX test loader aligned with the renderer's react-jsx build.
void React;

const PURPOSE_LABELS: Record<ModelPurpose, string> = {
  s2_planning: 'Plan tasks and propose actions using task text and current interface information',
  s1_teacher: 'Make semantic decisions using current interface information',
  computer_proposals: 'Send screenshots of the separately authorized app to OpenAI Responses for computer-use proposals. This does not authorize window access or action input.',
};

function defaultPurposes(provider: ModelProviderDefinition): ModelPurpose[] {
  return provider.auth === 'openai_oauth' ? ['s2_planning', 's1_teacher'] : ['s2_planning'];
}

function supportsComputerProposals(provider: ModelProviderDefinition): boolean {
  return provider.auth === 'api_key'
    && provider.route.provider_id === 'open_ai'
    && provider.route.protocol === 'open_ai_responses'
    && provider.route.base_url === 'https://api.openai.com/v1';
}

function providerLabel(provider: ModelProviderDefinition): string {
  return provider.auth === 'openai_oauth' ? `${provider.label} account` : provider.label;
}

function providerStatus(connections: ProviderConnection[]): { label: string; kind: 'ready' | 'saved' | 'warning' | 'empty' } {
  if (connections.some((connection) => connection.available)) return { label: 'Connected', kind: 'ready' };
  if (connections.some((connection) => connection.storage === 'keychain')) return { label: 'Saved', kind: 'saved' };
  if (connections.length > 0) return { label: 'Unavailable', kind: 'warning' };
  return { label: 'Not configured', kind: 'empty' };
}

function newestConnection(connections: ProviderConnection[]): ProviderConnection | undefined {
  return connections.reduce<ProviderConnection | undefined>((latest, connection) => {
    if (!latest) return connection;
    const latestTime = latest.refreshed_at ?? -1;
    const connectionTime = connection.refreshed_at ?? -1;
    return connectionTime >= latestTime ? connection : latest;
  }, undefined);
}

function sourceLabel(source: 'sdk' | 'live'): string {
  return source === 'live' ? 'Live provider directory' : 'Bundled SDK catalog';
}

function refreshedAtLabel(value: number | null): string {
  if (value === null) return 'Refresh date not provided';
  const milliseconds = value < 100_000_000_000 ? value * 1000 : value;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? 'Refresh date unavailable' : `Refreshed ${date.toLocaleString()}`;
}

function purposeLabel(purpose: ModelPurpose): string {
  return PURPOSE_LABELS[purpose] ?? purpose;
}

function formatModelStatus(model: ProviderModel): string {
  return model.selectable ? '' : 'Unavailable for task selection';
}

function Chevron({ direction }: { direction: 'left' | 'right' }) {
  return <svg className="provider-settings-chevron" viewBox="0 0 24 24" aria-hidden="true">
    <path d={direction === 'right' ? 'm9 18 6-6-6-6' : 'm15 18-6-6 6-6'} />
  </svg>;
}

export function ProviderSettingsPanel({
  snapshot,
  loadError,
  configure,
  refreshModels,
  removeConnection,
  cancelSignIn,
  initialProviderId,
}: {
  snapshot: ProviderSettingsSnapshot | null;
  loadError: string | null;
  configure(request: ConfigureProviderRequest): Promise<void>;
  refreshModels(id: string): Promise<void>;
  removeConnection(id: string): Promise<void>;
  cancelSignIn(): Promise<void>;
  /** Optional route entry for the provider blocking a selected task model. */
  initialProviderId?: string;
}) {
  const initialProvider = snapshot?.providers.some((provider) => provider.id === initialProviderId)
    ? initialProviderId ?? null
    : null;
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(initialProvider);
  const [key, setKey] = useState('');
  const [purposeDrafts, setPurposeDrafts] = useState<Record<string, ModelPurpose[]>>({});
  const [filter, setFilter] = useState('');
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [pendingOAuthReauth, setPendingOAuthReauth] = useState(false);
  const cancelRequested = useRef(false);
  const mountedRef = useRef(false);
  const transactionGenerationRef = useRef(0);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      ++transactionGenerationRef.current;
    };
  }, []);

  const provider = snapshot?.providers.find((item) => item.id === selectedProviderId) ?? null;
  const providerConnections = provider
    ? snapshot?.connections.filter((connection) => connection.provider_id === provider.id) ?? []
    : [];
  const savedOAuthConnection = provider?.auth === 'openai_oauth'
    ? providerConnections.find((connection) => connection.storage === 'keychain')
    : undefined;
  const keychainAvailable = snapshot?.keychain_available === true;
  const selectedPurposes = provider ? purposeDrafts[provider.id] ?? defaultPurposes(provider) : [];
  const purposeOptions = provider
    ? [
      { purpose: 's2_planning' as const, label: PURPOSE_LABELS.s2_planning },
      { purpose: 's1_teacher' as const, label: PURPOSE_LABELS.s1_teacher },
      ...(supportsComputerProposals(provider)
        ? [{ purpose: 'computer_proposals' as const, label: PURPOSE_LABELS.computer_proposals }]
        : []),
    ]
    : [];
  const isBusy = cancelling || pendingAction !== null || snapshot?.busy === true;
  const canConfigure = snapshot?.can_configure === true && keychainAvailable && !isBusy && provider !== null;

  const selectProvider = (providerId: string): void => {
    ++transactionGenerationRef.current;
    setSelectedProviderId(providerId);
    setKey('');
    setFilter('');
    setError(null);
    setNotice(null);
  };

  const showProviderList = (): void => {
    ++transactionGenerationRef.current;
    setSelectedProviderId(null);
    setKey('');
    setError(null);
    setNotice(null);
  };

  const setPurpose = (purpose: ModelPurpose, checked: boolean): void => {
    if (!provider) return;
    const next = checked
      ? [...selectedPurposes, purpose]
      : selectedPurposes.filter((item) => item !== purpose);
    setPurposeDrafts((current) => ({ ...current, [provider.id]: [...new Set(next)] }));
  };

  const startTransaction = <T,>(
    action: string,
    operation: () => Promise<T>,
    successMessage: string,
    failureMessage: string,
  ): void => {
    if (isBusy) return;
    const generation = ++transactionGenerationRef.current;
    setError(null);
    setNotice(null);
    setPendingAction(action);
    void runCurrentProviderTransaction(
      () => isCurrentCredentialTransaction(mountedRef.current, generation, transactionGenerationRef.current),
      operation,
      () => setNotice(successMessage),
      () => setError(failureMessage),
      () => setPendingAction(null),
    );
  };

  const startConfiguration = (reauthenticateOAuth = false): void => {
    if (!provider || !canConfigure) return;
    const allowedPurposes = new Set(purposeOptions.map((option) => option.purpose));
    const requestPurposes = selectedPurposes.filter((purpose) => allowedPurposes.has(purpose));
    if (requestPurposes.length === 0) {
      setError('Choose at least one purpose for this connection.');
      return;
    }
    if (provider.auth === 'api_key' && key.trim() === '') {
      setError('Enter the provider API key to continue.');
      return;
    }

    const request: ConfigureProviderRequest = keychainConfigureProviderRequest(provider.id, requestPurposes, {
      ...(provider.auth === 'openai_oauth' && reauthenticateOAuth ? { reauthenticate: true } : {}),
    });
    cancelRequested.current = false;
    const transaction = ++transactionGenerationRef.current;
    setError(null);
    setNotice(null);
    setPendingAction('configure');
    setPendingOAuthReauth(provider.auth === 'openai_oauth' && reauthenticateOAuth);

    const runConfigure = async (): Promise<void> => {
      if (provider.auth === 'openai_oauth') {
        await configure(request);
        return;
      }
      await persistProviderCredentialInput(key, async (credential) => {
        const credentialRequest = keychainConfigureProviderRequest(provider.id, requestPurposes, { apiKey: credential });
        try {
          await configure(credentialRequest);
        } finally {
          credentialRequest.api_key = '';
        }
      }, () => {
        if (mountedRef.current) setKey('');
      });
    };

    // Clear the renderer field immediately; the transaction helper clears it
    // again after persistence, including when Main reports a failure.
    setKey('');
    void runCurrentProviderTransaction(
      () => isCurrentCredentialTransaction(mountedRef.current, transaction, transactionGenerationRef.current),
      runConfigure,
      () => setNotice('Provider setup completed. Choose a task model in Tasks.'),
      () => {
        if (cancelRequested.current) setNotice('Sign-in cancelled. You can sign in again.');
        else setError('Provider setup could not be completed. Check the connection and try again.');
      },
      () => {
        setPendingAction(null);
        setPendingOAuthReauth(false);
      },
    );
  };

  const submit = (event: FormEvent): void => {
    event.preventDefault();
    startConfiguration();
  };

  const cancelLogin = async (): Promise<void> => {
    if (cancelling || cancelRequested.current || pendingAction !== 'configure' || provider?.auth !== 'openai_oauth') return;
    cancelRequested.current = true;
    setCancelling(true);
    try {
      await cancelSignIn();
    } catch {
      cancelRequested.current = false;
      if (mountedRef.current) setError('Could not cancel sign-in. Try again.');
    } finally {
      if (mountedRef.current) setCancelling(false);
    }
  };

  const runConnectionAction = (action: 'refresh' | 'remove', connection: ProviderConnection): void => {
    const operation = action === 'refresh' ? refreshModels : removeConnection;
    const success = action === 'refresh'
      ? 'Model directory refresh requested.'
      : provider?.auth === 'openai_oauth' ? 'Signed out and removed the saved account.' : 'Provider connection removed.';
    const failure = action === 'refresh'
      ? 'The model directory could not be refreshed. The current catalog remains visible.'
      : 'The provider connection could not be removed.';
    startTransaction(`${action}:${connection.id}`, () => operation(connection.id), success, failure);
  };

  const useSavedKey = (connection: ProviderConnection): void => {
    if (!provider || provider.auth !== 'api_key' || !snapshot?.can_configure || isBusy
      || connection.storage !== 'keychain' || connection.available) return;
    const request = keychainConfigureProviderRequest(provider.id, selectedPurposes);
    startTransaction('saved-key', () => configure(request),
      'Provider setup completed. Choose a task model in Tasks.',
      'The saved provider credential could not be loaded. Enter a new key or check Keychain access.');
  };

  const query = filter.trim().toLocaleLowerCase();
  const latestConnection = newestConnection(providerConnections);
  const directoryModels = latestConnection?.models ?? provider?.models ?? [];
  const directorySource = latestConnection?.catalog_source ?? 'sdk';
  const directoryRefreshedAt = latestConnection?.refreshed_at ?? null;
  const directoryReason = latestConnection?.reason ?? null;
  const matchingModels = directoryModels.filter((model) =>
    `${model.label} ${model.id}`.toLocaleLowerCase().includes(query));
  const hasConnections = providerConnections.length > 0;

  return <section className="provider-settings" aria-label="Provider connections">
    {loadError ? <p className="provider-settings-message" role="alert">{loadError}</p> : null}
    {snapshot?.reason ? <p className="provider-settings-message" role="alert">{snapshot.reason}</p> : null}
    {snapshot === null && !loadError ? <p className="provider-settings-message" role="status">Loading provider settings.</p> : null}
    {error ? <p className="provider-settings-message provider-settings-error" role="alert">{error}</p> : null}
    {notice ? <p className="provider-settings-message provider-settings-notice" role="status">{notice}</p> : null}
    {snapshot?.busy ? <p className="provider-settings-message" role="status">Updating provider settings…</p> : null}

    {snapshot && snapshot.providers.length === 0 ? <p>No providers are available from the installed SDK.</p> : null}

    {snapshot && snapshot.providers.length > 0 && !provider ? <div className="provider-settings-card">
      <div className="provider-settings-card-title">Providers</div>
      <div className="provider-settings-card-body">
        {snapshot.providers.map((item) => {
          const connections = snapshot.connections.filter((connection) => connection.provider_id === item.id);
          const status = providerStatus(connections);
          return <button
            key={item.id}
            type="button"
            data-testid={`provider-list-item-${item.id}`}
            disabled={isBusy}
            onClick={() => selectProvider(item.id)}
            className="provider-settings-provider-row"
            aria-label={`${providerLabel(item)}, ${status.label}`}
          >
            <span className="provider-settings-provider-copy">
              <span className="provider-settings-provider-title">{providerLabel(item)}</span>
              <span className="provider-settings-provider-description">{item.description}</span>
            </span>
            <span className={`provider-settings-provider-status provider-settings-status-${status.kind}`}>
              <span aria-hidden="true" className="provider-settings-status-dot" />
              {status.label}
            </span>
            <Chevron direction="right" />
          </button>;
        })}
      </div>
    </div> : null}

    {snapshot && provider ? <>
      <div className="provider-settings-detail-bar">
        <button
          type="button"
          data-testid="provider-list-back"
          disabled={isBusy}
          onClick={showProviderList}
          className="provider-settings-back"
        >
          <Chevron direction="left" />
          Providers
        </button>
        <span aria-hidden="true" className="provider-settings-detail-divider" />
        <span className="provider-settings-detail-name">{providerLabel(provider)}</span>
      </div>

      <div className="provider-settings-card">
        <div className="provider-settings-card-title">{providerLabel(provider)}</div>
        <div className="provider-settings-card-body">
          <div className="provider-settings-row provider-settings-row-center">
            <div className="provider-settings-row-copy">
              <div className="provider-settings-row-title">Provider</div>
              <div className="provider-settings-row-description">{provider.description}</div>
            </div>
            <span className={`provider-settings-provider-status provider-settings-status-${providerStatus(providerConnections).kind}`}>
              <span aria-hidden="true" className="provider-settings-status-dot" />
              {providerStatus(providerConnections).label}
            </span>
          </div>

          <div className="provider-settings-row provider-settings-row-center" role={!keychainAvailable ? 'alert' : undefined}>
            <div className="provider-settings-row-copy">
              <div className="provider-settings-row-title">Credential storage</div>
              <div className="provider-settings-row-description">
                {keychainAvailable
                  ? 'New API keys and account credentials are saved to macOS Keychain after storage succeeds.'
                  : 'macOS Keychain is unavailable. Saving an API key or signing in is disabled until secure storage is available.'}
              </div>
            </div>
            <span className="provider-settings-storage-value">
              {keychainAvailable ? 'macOS Keychain' : 'Unavailable'}
            </span>
          </div>

          <form className="provider-settings-form" onSubmit={submit}>
            {provider.auth === 'api_key' ? <div className="provider-settings-row provider-settings-row-center">
              <label className="provider-settings-row-copy" htmlFor="provider-api-key">
                <span className="provider-settings-row-title">API key</span>
                <span className="provider-settings-row-description">
                  {hasConnections ? 'Enter a new API key to replace the current credential.' : 'Enter the provider API key to create a connection.'}
                  {' '}Saved credentials are never displayed here. New keys are stored in macOS Keychain.
                </span>
              </label>
              <div className="provider-settings-row-control">
                <input
                  id="provider-api-key"
                  aria-label="Provider API key"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  disabled={!canConfigure}
                  value={key}
                  onChange={(event) => setKey(event.target.value)}
                  placeholder="Enter API key"
                />
              </div>
            </div> : <div className="provider-settings-row provider-settings-row-center">
              <div className="provider-settings-row-copy">
                <div className="provider-settings-row-title">Account sign-in</div>
                <div className="provider-settings-row-description">
                  Sign in with your {provider.label} account in the system browser. The account credential is saved in macOS Keychain after sign-in succeeds.
                </div>
              </div>
            </div>}

            <fieldset className="provider-settings-purpose-fieldset" disabled={!canConfigure}>
              <legend className="provider-settings-purpose-heading">Allow this provider to</legend>
              {purposeOptions.map(({ purpose, label }) => <label className="provider-settings-purpose-option" key={purpose}>
                <input type="checkbox" checked={selectedPurposes.includes(purpose)}
                  onChange={(event) => setPurpose(purpose, event.target.checked)} />
                <span>{label}</span>
              </label>)}
            </fieldset>

            <div className="provider-settings-row provider-settings-row-center">
              <div className="provider-settings-row-copy">
                <div className="provider-settings-row-title">Provider connection</div>
                <div className="provider-settings-row-description">
                  {provider.auth === 'openai_oauth'
                    ? savedOAuthConnection
                      ? 'Reconnect the saved account from macOS Keychain. Choose Re-login to authorize a different account.'
                      : 'A browser window will open to authorize this account. The credential is saved in macOS Keychain after sign-in succeeds.'
                  : 'Save your provider credential to macOS Keychain for the selected purposes.'}
                </div>
              </div>
              <div className="provider-settings-row-actions">
              {provider.auth === 'openai_oauth' && savedOAuthConnection && !savedOAuthConnection.available ? (
                <button type="button" className="provider-settings-button"
                  disabled={!canConfigure || selectedPurposes.length === 0}
                  onClick={() => startConfiguration()}>
                  {pendingAction === 'configure' && !pendingOAuthReauth ? 'Connecting saved account…' : 'Connect saved account'}
                </button>
              ) : null}
              <button type={provider.auth === 'openai_oauth' ? 'button' : 'submit'}
                className="provider-settings-button provider-settings-button-primary"
                disabled={!canConfigure || selectedPurposes.length === 0 || (provider.auth === 'api_key' && key.trim() === '')}
                onClick={provider.auth === 'openai_oauth' ? () => startConfiguration(Boolean(savedOAuthConnection)) : undefined}>
                {pendingAction === 'configure'
                  ? provider.auth === 'openai_oauth'
                    ? pendingOAuthReauth ? 'Waiting for sign-in…' : 'Connecting…'
                    : 'Connecting…'
                  : provider.auth === 'openai_oauth'
                    ? savedOAuthConnection ? 'Re-login' : `Sign in with ${provider.label}`
                    : 'Save provider connection'}
              </button>
              {provider.auth === 'openai_oauth' && pendingAction === 'configure' ?
                <button type="button" className="provider-settings-button" disabled={cancelling || cancelRequested.current}
                  onClick={() => void cancelLogin()}>{cancelling || cancelRequested.current ? 'Cancelling…' : 'Cancel sign-in'}</button> : null}
              </div>
            </div>
          </form>

          {providerConnections.map((connection) => <div className="provider-settings-row provider-settings-row-center"
            key={connection.id}>
            <div className="provider-settings-row-copy">
              <div className="provider-settings-row-title">
                {connection.available ? 'Connected' : 'Unavailable'}
              </div>
              <div className="provider-settings-row-description">
                {connection.storage === 'keychain' ? 'Credential saved in macOS Keychain' : 'Credential is not saved in macOS Keychain'}
                {' · Allowed purposes: '}{connection.purposes.map(purposeLabel).join(', ') || 'none'}
                {connection.reason ? ` · ${connection.reason}` : ''}
              </div>
            </div>
            <div className="provider-settings-row-actions">
              {provider.auth === 'api_key' && connection.storage === 'keychain' && !connection.available ? <button type="button"
                className="provider-settings-button" disabled={!canConfigure || selectedPurposes.length === 0}
                onClick={() => useSavedKey(connection)}>
                {pendingAction === 'saved-key' ? 'Reconnecting…' : 'Reconnect saved key'}
              </button> : null}
              {provider.directory_supported && connection.available ? <button type="button"
                className="provider-settings-button" disabled={isBusy}
                onClick={() => runConnectionAction('refresh', connection)}>
                {pendingAction === `refresh:${connection.id}` ? 'Refreshing…' : 'Refresh directory'}
              </button> : null}
              <button type="button" className="provider-settings-button provider-settings-button-danger" disabled={isBusy}
                aria-label={provider.auth === 'openai_oauth' ? `Sign out of ${provider.label} account` : `Remove ${provider.label} connection`}
                onClick={() => runConnectionAction('remove', connection)}>
                {pendingAction === `remove:${connection.id}` ? 'Removing…' : provider.auth === 'openai_oauth' ? 'Sign out' : 'Remove'}
              </button>
            </div>
          </div>)}
        </div>
      </div>

      <div className="provider-settings-card">
        <div className="provider-settings-card-title">Model directory</div>
        <div className="provider-settings-card-body">
          <div className="provider-settings-row provider-settings-directory-toolbar">
            <div className="provider-settings-row-copy">
              <div className="provider-settings-row-title">Available models</div>
              <div className="provider-settings-row-description">Read-only list for {provider.label}. Model selection stays in Tasks.</div>
            </div>
            <label className="provider-settings-filter-label">
              <span>Search models</span>
              <input type="search" aria-label="Search model directory" value={filter}
                onChange={(event) => setFilter(event.target.value)} placeholder="Search models" />
            </label>
          </div>
          <div className="provider-settings-directory-meta">
            <span>{sourceLabel(directorySource)}</span>
            <span>{refreshedAtLabel(directoryRefreshedAt)}</span>
          </div>
          {directoryReason ? <p className="provider-settings-directory-reason" role="status">{directoryReason}</p> : null}
          {matchingModels.length > 0 ? <ul className="provider-settings-model-list">
            {matchingModels.map((model) => <li key={`${latestConnection?.id ?? provider.id}:${model.id}`}>
              <span className="provider-settings-model-label">{model.label}</span>
              <code>{model.id}</code>
              {formatModelStatus(model) ? <small>{formatModelStatus(model)}</small> : null}
            </li>)}
          </ul> : query ? <p className="provider-settings-empty">No matching models.</p>
            : <p className="provider-settings-empty">No models are listed in this catalog.</p>}
        </div>
      </div>
    </> : null}
  </section>;
}

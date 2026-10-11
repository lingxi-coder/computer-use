import React, { useState, type FormEvent } from 'react';
import type { ModelAuthorizationRequest, ModelAuthorizationStatus, ModelProvider, ModelPurpose, ModelRoute, OpenAiLoginRequest } from '@computer-use/desktop-api';

// The Node test loader uses classic JSX while the renderer build uses react-jsx.
void React;

export function ModelConnectionPanel({ status }: { status: ModelAuthorizationStatus }) {
  const [provider, setProvider] = useState('open_ai');
  const [protocol, setProtocol] = useState<ModelRoute['protocol']>('open_ai_responses');
  const [endpoint, setEndpoint] = useState('https://api.openai.com/v1');
  const [name, setName] = useState('');
  const [model, setModel] = useState('');
  const [openAiModel, setOpenAiModel] = useState('gpt-6-luna');
  const [key, setKey] = useState('');
  const [remember, setRemember] = useState(false);
  const [planningPurpose, setPlanningPurpose] = useState(true);
  const [teacherPurpose, setTeacherPurpose] = useState(false);
  const [computerPurpose, setComputerPurpose] = useState(false);
  const [openAiPlanningPurpose, setOpenAiPlanningPurpose] = useState(true);
  const [openAiTeacherPurpose, setOpenAiTeacherPurpose] = useState(true);
  const [openAiLoginPending, setOpenAiLoginPending] = useState(false);
  const [cancellingOpenAiLogin, setCancellingOpenAiLogin] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busy = submitting || openAiLoginPending || status.phase === 'authorizing' || status.phase === 'switching';
  const computerRouteAllowed = provider === 'open_ai' && protocol === 'open_ai_responses' && endpoint.trim() === 'https://api.openai.com/v1';
  const purposeLabel = (purpose: ModelPurpose): string => purpose === 's2_planning' ? 'S2 task planning'
    : purpose === 's1_teacher' ? 'S1 semantic decisions' : 'Computer-use proposals (screenshots only)';
  const title = status.phase === 'authorized'
    ? status.credential_kind === 'openai_oauth' ? 'OpenAI account session configured'
      : status.authorized_purposes.includes('s2_planning') ? 'Planning allowed'
      : status.authorized_purposes.includes('s1_teacher') ? 'Semantic decisions allowed' : 'Computer proposals allowed'
    : status.phase === 'authorizing' ? openAiLoginPending ? 'OpenAI account sign-in in progress' : 'Waiting for your permission'
    : status.phase === 'switching' ? 'Changing connection…' : status.phase === 'unavailable' ? 'Connection unavailable'
      : status.planning_available ? 'Demo planning' : 'No planning model';
  const invoke = async (call: () => Promise<void>, failure = 'Connection was cancelled or unavailable. Check its settings and try again.'): Promise<void> => {
    if (submitting) return; setSubmitting(true); setError(null);
    try { await call(); } catch { setError(failure); }
    finally { setSubmitting(false); }
  };
  const submitOpenAiLogin = (event: FormEvent): void => {
    event.preventDefault();
    if (busy || !status.can_authorize) return;
    const purposes: ModelPurpose[] = [
      ...(openAiPlanningPurpose ? ['s2_planning' as const] : []),
      ...(openAiTeacherPurpose ? ['s1_teacher' as const] : []),
    ];
    if (purposes.length === 0) { setError('Select at least one model purpose.'); return; }
    const request: OpenAiLoginRequest = { model: openAiModel.trim(), purposes };
    setOpenAiLoginPending(true);
    void invoke(async () => {
      try { await window.desktop.authorizeOpenAiLogin(request); }
      finally { request.purposes.length = 0; setOpenAiLoginPending(false); }
    }, 'OpenAI sign-in was cancelled or unavailable. No credential was shown.');
  };
  const cancelOpenAiLogin = (): void => {
    if (!openAiLoginPending || cancellingOpenAiLogin) return;
    setCancellingOpenAiLogin(true); setError(null);
    void window.desktop.revokeModel().catch(() => {
      setError('OpenAI sign-in could not be cancelled. No credential was shown.');
    }).finally(() => setCancellingOpenAiLogin(false));
  };
  const chooseProvider = (value: string): void => {
    setProvider(value); setKey('');
    if (value === 'anthropic_first_party') { setProtocol('anthropic_messages'); setEndpoint('https://api.anthropic.com'); }
    else if (value === 'open_ai') { setProtocol('open_ai_responses'); setEndpoint('https://api.openai.com/v1'); }
    else { setProtocol('open_ai_chat'); setEndpoint(''); }
    setComputerPurpose(false);
  };
  const submit = (event: FormEvent): void => {
    event.preventDefault(); if (busy || !status.can_authorize) return;
    const purposes: ModelPurpose[] = [
      ...(planningPurpose ? ['s2_planning' as const] : []),
      ...(teacherPurpose ? ['s1_teacher' as const] : []),
      ...(computerPurpose ? ['computer_proposals' as const] : []),
    ];
    if (purposes.length === 0) { setError('Select at least one model purpose.'); return; }
    if (computerPurpose && !computerRouteAllowed) { setError('Computer proposals require the official OpenAI Responses endpoint.'); return; }
    const id: ModelProvider = provider === 'compatible' ? { open_ai_compatible: { name: name.trim() } }
      : provider === 'custom' ? { custom: { name: name.trim() } } : provider as 'open_ai' | 'anthropic_first_party';
    const request: ModelAuthorizationRequest = { route: { provider_id: id, protocol, base_url: endpoint.trim(), model: model.trim(), max_output_tokens: 4096 }, api_key: key, storage: remember ? 'keychain' : 'memory', purposes };
    setKey(''); // never keep a submitted credential in the form or projection
    void invoke(async () => { try { await window.desktop.authorizeModel(request); } finally { request.api_key = ''; } });
  };
  return <section className="panel model-connection" aria-labelledby="model-connection-title">
    <div className="model-heading"><h2 id="model-connection-title">Model connection</h2><span className={`connection-status ${status.phase}`} role="status"><i aria-hidden="true" />{title}</span></div>
    {provider === 'open_ai' ? <form className="connection-form openai-account-login" aria-label="OpenAI account login" onSubmit={submitOpenAiLogin}>
      <h3>Sign in with your OpenAI account</h3>
      <p>Continue in your system browser. The account session is stored in this app’s Keychain; purpose consent is requested again after restarting the app.</p>
      <label>Model identifier<input aria-label="OpenAI account model identifier" required autoComplete="off" spellCheck={false} disabled={busy || !status.can_authorize} value={openAiModel} onChange={(event) => setOpenAiModel(event.target.value)} /></label>
      <fieldset className="model-purposes" disabled={busy || !status.can_authorize}>
        <legend>Allow this model to</legend>
        <label><input aria-label="OpenAI account S2 task planning purpose" type="checkbox" checked={openAiPlanningPurpose} onChange={(event) => setOpenAiPlanningPurpose(event.target.checked)} />Plan tasks and propose actions using task text and current interface/target information</label>
        <label><input aria-label="OpenAI account S1 semantic decision purpose" type="checkbox" checked={openAiTeacherPurpose} onChange={(event) => setOpenAiTeacherPurpose(event.target.checked)} />Make semantic decisions using current interface/target information</label>
      </fieldset>
      <button type="submit" className="primary" disabled={busy || !status.can_authorize || !openAiModel.trim() || (!openAiPlanningPurpose && !openAiTeacherPurpose)}>Continue with OpenAI account…</button>
      {openAiLoginPending ? <div role="status">Waiting for OpenAI sign-in to finish in your browser.</div> : null}
      {openAiLoginPending ? <button type="button" disabled={cancellingOpenAiLogin} onClick={cancelOpenAiLogin}>{cancellingOpenAiLogin ? 'Cancelling sign-in…' : 'Cancel OpenAI sign-in'}</button> : null}
    </form> : null}
    {status.route ? <div className="model-identity"><strong>{status.route.model}</strong><span title={status.route.base_url}>{status.route.base_url}</span>
      {status.credential_kind === 'openai_oauth'
        ? <small>OpenAI account session stored in this app’s Keychain. Purpose consent is requested again after restarting.</small>
        : <small>{status.storage === 'keychain' ? 'Key saved on this Mac' : 'API key kept for this run'}</small>}
      {status.authorized_purposes.length > 0 ? <small>Authorized purposes: {status.authorized_purposes.map(purposeLabel).join(' · ')}</small> : null}
      {status.authorized_purposes.includes('computer_proposals') ? <small>Model-purpose approval does not authorize a window; select and separately authorize a window for this session before screenshots can be sent.</small> : null}</div> : null}
    {status.phase === 'authorized' || status.phase === 'unavailable'
      ? <button type="button" className="connection-revoke" disabled={submitting} onClick={() => void invoke(async () => {
        await window.desktop.revokeModel(); setPlanningPurpose(true); setTeacherPurpose(false); setComputerPurpose(false);
      })}>Revoke connection and cancel task</button> : null}
    {status.saved.length > 0 ? <div className="saved-connections" aria-label="Saved model connections">
      {status.saved.map((saved) => <div className="saved-connection" key={saved.id}><div><strong>{saved.route.model}</strong><span>{new URL(saved.route.base_url).hostname}</span></div>
        <button type="button" disabled={busy || !status.can_authorize || (!planningPurpose && !teacherPurpose && !computerPurpose)
          || (computerPurpose && !(saved.route.provider_id === 'open_ai' && saved.route.protocol === 'open_ai_responses' && saved.route.base_url === 'https://api.openai.com/v1'))} onClick={() => void invoke(() => window.desktop.authorizeSavedModel(saved.id, [
          ...(planningPurpose ? ['s2_planning' as const] : []), ...(teacherPurpose ? ['s1_teacher' as const] : []),
          ...(computerPurpose ? ['computer_proposals' as const] : []),
        ]))}>Use saved key…</button>
        <button type="button" disabled={busy} onClick={() => void invoke(() => window.desktop.forgetSavedModel(saved.id))}>Forget</button></div>)}
    </div> : null}
    <details className="connection-form" open={status.phase === 'not_configured' && !status.planning_available}>
      <summary>{status.route ? 'Change connection' : 'Configure a model'}</summary>
      <form onSubmit={submit}>
        <fieldset disabled={busy || !status.can_authorize}>
          <label>Service<select aria-label="Model service" value={provider} onChange={(event) => chooseProvider(event.target.value)}>
            <option value="open_ai">OpenAI</option><option value="anthropic_first_party">Anthropic</option><option value="compatible">OpenAI-compatible</option><option value="custom">Custom service</option>
          </select></label>
          {provider === 'compatible' || provider === 'custom' ? <label>Service name<input aria-label="Service name" required value={name} maxLength={128} onChange={(event) => setName(event.target.value)} /></label> : null}
          {provider !== 'anthropic_first_party' ? <label>Protocol<select aria-label="Model protocol" value={protocol} onChange={(event) => {
            const value = event.target.value as ModelRoute['protocol']; setProtocol(value);
            if (value !== 'open_ai_responses' || provider !== 'open_ai') setComputerPurpose(false);
          }}>
            <option value="open_ai_responses">OpenAI Responses</option><option value="open_ai_chat">OpenAI Chat</option>{provider === 'custom' ? <option value="anthropic_messages">Anthropic Messages</option> : null}
          </select></label> : null}
          <label>HTTPS endpoint<input aria-label="Model endpoint" type="url" required autoComplete="off" spellCheck={false} value={endpoint} onChange={(event) => {
            const value = event.target.value; setEndpoint(value);
            if (value.trim() !== 'https://api.openai.com/v1') setComputerPurpose(false);
          }} /></label>
          <label>Model identifier<input aria-label="Model identifier" required autoComplete="off" spellCheck={false} value={model} placeholder="Enter the model identifier from your service" onChange={(event) => setModel(event.target.value)} /></label>
          <label>API key<input aria-label="Model API key" type="password" required autoComplete="new-password" spellCheck={false} value={key} onChange={(event) => setKey(event.target.value)} /></label>
          <fieldset className="model-purposes">
            <legend>Allow this model to</legend>
            <label><input aria-label="S2 task planning purpose" type="checkbox" checked={planningPurpose} onChange={(event) => setPlanningPurpose(event.target.checked)} />Plan tasks and propose actions using task text and current interface/target information</label>
            <label><input aria-label="S1 semantic decision purpose" type="checkbox" checked={teacherPurpose} onChange={(event) => setTeacherPurpose(event.target.checked)} />Make semantic decisions using current interface/target information</label>
            <label><input aria-label="Computer proposals purpose" type="checkbox" checked={computerPurpose} disabled={!computerRouteAllowed} onChange={(event) => setComputerPurpose(event.target.checked)} />Send full screenshots of the authorized application, including visible window content, to OpenAI Responses (https://api.openai.com/v1) for computer-use proposals; this does not allow input or action execution</label>
            <small>{computerRouteAllowed
              ? 'Model-purpose approval alone does not authorize a window. Select and separately authorize a window for this session before screenshots can be sent.'
              : 'Computer-use proposals are available only with OpenAI Responses at https://api.openai.com/v1.'}</small>
          </fieldset>
          <label className="remember-key"><input type="checkbox" checked={remember} disabled={!status.can_save} onChange={(event) => setRemember(event.target.checked)} />Save API key in this app’s Keychain</label>
          <p className="connection-copy">For selected purposes, task text, registered controls and current interface/target information may be sent to this service. {computerPurpose ? 'This includes screenshots of the authorized application for computer-use proposals; it does not grant input or execution permission, and a separate window authorization is required before screenshots can be sent. ' : ''}Keychain storage saves only the key and route; purpose consent is requested again when reconnecting.</p>
          <button type="submit" className="primary" disabled={!model.trim() || !key || !endpoint.trim() || (!planningPurpose && !teacherPurpose && !computerPurpose)}>Review and allow selected purposes…</button>
        </fieldset>
      </form>
    </details>
    {status.reason ? <div className="source-reason">{status.reason}</div> : null}
    {error ? <div className="source-reason" role="alert">{error}</div> : null}
  </section>;
}

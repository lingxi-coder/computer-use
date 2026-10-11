import React, { useRef, useState } from 'react';
import type { ModelAuthorizationStatus, OwnedDocumentStatus } from '@computer-use/desktop-api';

// The Node test loader uses classic JSX while the renderer build uses react-jsx.
void React;

export function hasS2PlanningAuthorization(status: ModelAuthorizationStatus | null): boolean {
  return status?.phase === 'authorized' && status.authorized_purposes.includes('s2_planning');
}

export function ownedDocumentExists(status: OwnedDocumentStatus | null): boolean {
  return status !== null && status.phase !== 'not_created';
}

function hasRegisteredDocument(status: OwnedDocumentStatus): boolean {
  return status.phase === 'observing' || status.phase === 'authorizing_edit'
    || status.phase === 'edit_authorized' || status.phase === 'authorizing_save'
    || status.phase === 'save_authorized' || status.phase === 'revisiting'
    || status.phase === 'completed';
}

export function ownedDocumentPlanningAllowed(
  status: OwnedDocumentStatus | null,
  model: ModelAuthorizationStatus | null,
): boolean {
  return !ownedDocumentExists(status)
    || (status?.phase === 'save_authorized' && hasS2PlanningAuthorization(model));
}

export function ownedDocumentBusy(status: OwnedDocumentStatus | null): boolean {
  return status !== null && (status.phase === 'creating' || status.phase === 'authorizing_edit'
    || status.phase === 'authorizing_save' || status.phase === 'revisiting');
}

function phaseLabel(phase: OwnedDocumentStatus['phase']): string {
  switch (phase) {
    case 'not_created': return 'No temporary document created';
    case 'creating': return 'Creating a temporary TextEdit document…';
    case 'observing': return 'Temporary document is ready for edit permission';
    case 'authorizing_edit': return 'Waiting for edit permission';
    case 'edit_authorized': return 'Editing is allowed for this temporary document';
    case 'authorizing_save': return 'Waiting for Save permission';
    case 'save_authorized': return 'Save permission granted; the document is not yet verified';
    case 'revisiting': return 'Reopening the temporary document for verification…';
    case 'completed': return 'The reopened document was verified';
    case 'unavailable': return 'Temporary document unavailable';
  }
}

export function OwnedDocumentPanel({
  status,
  planningAuthorized,
  create,
  authorizeEdit,
  authorizeSave,
  revisit,
  revoke,
}: {
  status: OwnedDocumentStatus;
  planningAuthorized: boolean;
  create: () => Promise<void>;
  authorizeEdit: () => Promise<void>;
  authorizeSave: () => Promise<void>;
  revisit: () => Promise<void>;
  revoke: () => Promise<void>;
}) {
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [cancelBusy, setCancelBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const cancelGeneration = useRef(0);
  const busy = busyAction !== null || cancelBusy || ownedDocumentBusy(status);
  const exists = status.phase !== 'not_created';
  const registered = hasRegisteredDocument(status);
  const canRevoke = status.phase === 'authorizing_edit' || status.phase === 'edit_authorized'
    || status.phase === 'authorizing_save' || status.phase === 'save_authorized';

  const invoke = async (name: string, action: () => Promise<void>): Promise<void> => {
    if (busy) return;
    const startedAtCancelGeneration = cancelGeneration.current;
    setBusyAction(name);
    setError(null);
    try { await action(); }
    catch {
      if (startedAtCancelGeneration === cancelGeneration.current) {
        setError(name === 'create' ? 'The temporary document could not be created.'
          : name === 'edit' ? 'Edit permission was cancelled or unavailable.'
            : name === 'save' ? 'Save permission was cancelled or unavailable.'
              : 'The temporary document could not be verified.');
      }
    }
    finally { setBusyAction(null); }
  };

  const cancelPermission = async (): Promise<void> => {
    if (cancelBusy) return;
    cancelGeneration.current += 1;
    setCancelBusy(true);
    setCancelError(null);
    try { await revoke(); }
    catch { setCancelError('Editing and Save permission could not be revoked.'); }
    finally { setCancelBusy(false); }
  };

  return <section className="panel owned-document" aria-labelledby="owned-document-title">
    <h2 id="owned-document-title">Temporary TextEdit document</h2>
    <p className="owned-document-copy">
      Computer Use creates and manages a private temporary document in TextEdit for this flow. It does not select or edit an existing document.
      Editing and saving each require separate native permission. Save permission does not mean the document has been saved or verified.
    </p>
    <div className="owned-document-state" role="status">{phaseLabel(status.phase)}</div>
    {status.reason ? <div className="source-reason">{status.reason}</div> : null}
    {exists && (status.phase !== 'save_authorized' || !planningAuthorized) ? <div className="owned-document-planning" role="status">
      Approve editing and saving for this temporary document, then configure and authorize an S2 planning model before starting or confirming a task or enabling Execute. Creating the document and approving its edit remain available without S2 planning.
    </div> : null}
    <div className="owned-document-actions">
      <button type="button" className="primary" disabled={busy || !status.can_create}
        onClick={() => void invoke('create', create)}>Create temporary document</button>
      <button type="button" disabled={busy || !registered || !status.can_authorize_edit}
        onClick={() => void invoke('edit', authorizeEdit)}>Allow editing…</button>
      <button type="button" disabled={busy || !registered || !status.can_authorize_save || !planningAuthorized}
        onClick={() => void invoke('save', authorizeSave)}>Allow saving…</button>
      <button type="button" disabled={busy || !registered || !status.can_revisit}
        onClick={() => void invoke('revisit', revisit)}>Reopen to verify</button>
    </div>
    {canRevoke ? <button type="button" className="owned-document-revoke" disabled={cancelBusy}
      onClick={() => void cancelPermission()}>
      {status.phase === 'authorizing_edit' || status.phase === 'authorizing_save'
        ? 'Cancel permission request' : 'Revoke editing and saving'}
    </button> : null}
    {error ? <div className="source-reason" role="alert">{error}</div> : null}
    {cancelError ? <div className="source-reason" role="alert">{cancelError}</div> : null}
  </section>;
}

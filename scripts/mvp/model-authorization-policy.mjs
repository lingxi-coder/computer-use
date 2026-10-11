const REQUIRED_PURPOSES = Object.freeze(['s2_planning', 's1_teacher']);
const PURPOSE_LABELS = Object.freeze(['S2 planning', 'S1 teacher']);

export function productModelAuthorizationReady(status) {
  if (!status || status.phase !== 'authorized' || status.planning_available !== true
      || !Array.isArray(status.authorized_purposes) || status.authorized_purposes.length !== REQUIRED_PURPOSES.length) return false;
  return REQUIRED_PURPOSES.every(purpose => status.authorized_purposes.includes(purpose))
    && !status.authorized_purposes.includes('computer_proposals');
}

export function productPurposeConsentMatches(detail) {
  if (typeof detail !== 'string' || detail.includes('Computer actions:')) return false;
  const marker = 'Selected purposes:\n';
  const start = detail.indexOf(marker);
  if (start < 0) return false;
  const section = detail.slice(start + marker.length).split('\n\n', 1)[0];
  const labels = section.split('\n').map(line => /^• (S2 planning|S1 teacher):/u.exec(line)?.[1] ?? null);
  return labels.length === PURPOSE_LABELS.length && labels.every((label, index) => label === PURPOSE_LABELS[index]);
}

export function nativeConsentDriverArgs(action, auditKind) {
  if (action !== 'allow' && action !== 'cancel') throw new Error('Unsupported native consent action.');
  const helperKind = auditKind === 'model' ? 'model' : auditKind === 'fixture_window' ? 'window' : null;
  if (!helperKind) throw new Error('Unsupported native consent dialog kind.');
  return [action, helperKind];
}

export function summarizeProductModelAuthorization(status) {
  return {
    phase: typeof status?.phase === 'string' ? status.phase : 'unknown',
    planning_available: status?.planning_available === true,
    authorized_purposes: Array.isArray(status?.authorized_purposes)
      ? status.authorized_purposes.filter(purpose => typeof purpose === 'string') : [],
    saved_connection_count: Array.isArray(status?.saved) ? status.saved.length : 0,
    storage: typeof status?.storage === 'string' ? status.storage : null,
    reason: typeof status?.reason === 'string'
      ? status.reason.replace(/[\u0000-\u001f\u007f]/g, ' ').slice(0, 512) : null,
  };
}

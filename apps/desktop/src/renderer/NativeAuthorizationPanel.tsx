import type { NativeAuthorization } from '@computer-use/desktop-api';

export function NativeAuthorizationPanel({ status, authorize, refresh, revoke }: {
  status: NativeAuthorization;
  authorize: (snapshotId: string) => void;
  refresh: () => void;
  revoke: () => void;
}) {
  if (status.phase === 'not_native') return null;
  const target = status.authorized ?? status.candidate;
  const isBackgroundAx = (status.authorized_domain ?? status.candidate_domain) === 'background_ax';
  const busy = status.phase === 'authorizing' || status.phase === 'switching';
  const label = status.phase === 'authorized' ? (isBackgroundAx ? 'Background AXPress allowed' : 'Window allowed') : status.phase === 'authorizing' ? 'Waiting for your permission'
    : status.phase === 'switching' ? 'Changing window permission…' : status.phase === 'unavailable' ? 'Session unavailable' : 'Observation only';
  return (
    <section className="panel native-authorization" aria-labelledby="native-authorization-title">
      <h2 id="native-authorization-title">Window permission</h2>
      <div role="status">{label}</div>
      {target ? <div className="observation-target"><strong>{target.window_title ?? 'Untitled window'}</strong><span>{target.app}</span></div>
        : <div className="source-pending">Waiting for a current window in a supported application.</div>}
      {isBackgroundAx
        ? status.phase === 'authorized'
          ? <p>One AXPress on a verified target in this background window is allowed. Foreground controls, text, keyboard and scrolling remain unavailable.</p>
          : <p>Allow one AXPress on a verified target in this background window. This does not grant foreground input, text, keyboard or scrolling.</p>
        : status.phase === 'authorized'
          ? <p>Ordinary text and registered controls in this window are allowed. Confirm a task and choose Execute to run it.</p>
          : <p>Allow this window to read ordinary text and edit registered controls. You can revoke permission at any time.</p>}
      {status.reason ? <div className="source-reason">{status.reason}</div> : null}
      <div className="row">
        {status.phase === 'authorized'
          ? <button type="button" onClick={revoke}>Revoke permission and cancel task</button>
          : status.can_authorize
            ? <button type="button" className="primary" disabled={busy} onClick={() => target && authorize(target.snapshot_id)}>
              {isBackgroundAx ? 'Allow one AXPress…' : 'Allow this window…'}
            </button>
            : null}
        {status.can_refresh_observation ? <button type="button" aria-label="Refresh observation" disabled={busy} onClick={refresh}>Refresh observation</button> : null}
        {status.phase === 'authorizing' ? <button type="button" onClick={revoke}>Cancel</button> : null}
      </div>
    </section>
  );
}

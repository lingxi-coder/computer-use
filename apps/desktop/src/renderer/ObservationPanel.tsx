import type { ObservationDisplay } from './viewModel';

export function ObservationPanel({ observation }: { observation: ObservationDisplay }) {
  return (
    <section className="panel observation" aria-labelledby="observation-title">
      <div className="observation-heading">
        <h2 id="observation-title">Observation</h2>
        {observation.readOnly ? <span className="read-only">Read only</span> : null}
      </div>
      <div className={`observation-state ${observation.level}`} role="status">{observation.title}</div>
      {observation.app || observation.window ? (
        <div className="observation-target">
          {observation.app ? <strong>{observation.app}</strong> : null}
          {observation.window ? <span>{observation.window}</span> : null}
        </div>
      ) : null}
      <div className="sources" aria-label="Observation sources">
        {observation.sources.length === 0 ? <span className="source-pending">Waiting for source status</span> : null}
        {observation.sources.map((source) => (
          <div className={`source ${source.state}`} key={source.label}>
            <div className="source-line"><span>{source.label}</span><span className="source-state">{source.stateLabel}</span></div>
            {source.reason ? <div className="source-reason">{source.reason}</div> : null}
          </div>
        ))}
      </div>
      {observation.frame ? (
        <dl className="frame-facts">
          <div><dt>Image</dt><dd>{observation.frame.status}{observation.frame.healthy ? '' : ' · freshness unconfirmed'}</dd></div>
          <div><dt>Size</dt><dd>{observation.frame.dimensions}</dd></div>
          <div><dt>Image age</dt><dd>{observation.frame.imageAge}</dd></div>
          <div><dt>Capture checked</dt><dd>{observation.frame.confirmationAge}</dd></div>
          <div><dt>Dropped frames</dt><dd>{observation.frame.dropped}</dd></div>
        </dl>
      ) : <div className="source-pending">No image received yet</div>}
    </section>
  );
}

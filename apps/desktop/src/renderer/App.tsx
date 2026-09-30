import { useEffect, useMemo, useState } from 'react';

import type { BridgeStopStatus, RunMode, UiProjection } from '@computer-user/desktop-api';

import { toViewModel } from './viewModel';

const MODES: { value: RunMode; label: string }[] = [
  { value: 'observe', label: 'Observe only' },
  { value: 'shadow', label: 'Shadow (suggest, do nothing)' },
  { value: 'execute', label: 'Execute' },
];

export function App() {
  const [projection, setProjection] = useState<UiProjection | null>(null);
  const [bridgeStop, setBridgeStop] = useState<BridgeStopStatus>('idle');
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    void window.desktop.snapshot().then((snapshot) => {
      if (current && snapshot) setProjection(snapshot);
    });
    const offProjection = window.desktop.onProjection(setProjection);
    const offStop = window.desktop.onStopStatus(setBridgeStop);
    return () => {
      current = false;
      offProjection();
      offStop();
    };
  }, []);

  const view = useMemo(() => toViewModel(projection, bridgeStop), [projection, bridgeStop]);

  // A refused or failed call is shown, not swallowed.
  const run = (call: () => Promise<void>): void => {
    setError(null);
    call().catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)));
  };

  return (
    <main className="app">
      <header className="bar">
        <span className="device" title="The device this app is driving">
          {view.capability}
        </span>
        <label className="mode">
          <span>Mode</span>
          <select value={view.mode} onChange={(event) => run(() => window.desktop.setMode(event.target.value as RunMode))}>
            {MODES.map((mode) => (
              <option key={mode.value} value={mode.value}>
                {mode.label}
              </option>
            ))}
          </select>
        </label>
      </header>

      <section className="panel">
        <h1>What should it do?</h1>
        <textarea
          aria-label="Task"
          rows={3}
          value={text}
          placeholder={'Turn off "Daily picks" in "Push"'}
          onChange={(event) => setText(event.target.value)}
        />
        <button
          type="button"
          className="primary"
          disabled={!view.canCreate || text.trim() === ''}
          onClick={() => {
            run(() => window.desktop.createTask(text.trim()));
            setText('');
          }}
        >
          Start
        </button>
      </section>

      {view.prompt && (
        <section className="panel prompt" role="alertdialog" aria-labelledby="prompt-title">
          <h2 id="prompt-title">{view.prompt.title}</h2>
          <ul>
            {view.prompt.lines.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
          <div className="row">
            <button type="button" className="primary" onClick={() => run(() => window.desktop.confirm(view.prompt!.id, true))}>
              {view.prompt.kind === 'goal' ? 'Yes, do it' : 'Allow this action'}
            </button>
            <button type="button" onClick={() => run(() => window.desktop.confirm(view.prompt!.id, false))}>
              No
            </button>
          </div>
        </section>
      )}

      <section className="panel" aria-live="polite">
        <div className="state">{view.stateLabel}</div>
        {view.headline && <div className="headline">{view.headline}</div>}
        {view.progress && (
          <progress max={view.progress.max} value={view.progress.used} aria-label="Attempts used" />
        )}
        <div className="row">
          <button type="button" disabled={!view.canPause} onClick={() => run(() => window.desktop.pause(view.rootTaskId!))}>
            Pause
          </button>
          <button type="button" disabled={!view.canResume} onClick={() => run(() => window.desktop.resume(view.rootTaskId!))}>
            Resume
          </button>
        </div>
      </section>

      <section className="panel">
        <button type="button" className="stop" disabled={!view.canStop} onClick={() => run(() => window.desktop.stop())}>
          Stop
        </button>
        {view.stop.level !== 'none' && (
          <div className={`banner ${view.stop.level}`} role="status">
            {view.stop.text}
          </div>
        )}
      </section>

      {error && (
        <div className="banner warning" role="alert">
          {error}
        </div>
      )}

      {view.timeline.length > 0 && (
        <section className="panel timeline" aria-label="What happened">
          <ol>
            {view.timeline.map((entry, index) => (
              <li key={`${index}-${entry}`}>{entry}</li>
            ))}
          </ol>
        </section>
      )}
    </main>
  );
}

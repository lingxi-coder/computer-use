const DEFAULT_TIMEOUT_MS = 15_000;

/** Resolve only after Main's normal ready-to-show path has produced a visible window. */
export function waitForNormalMainWindowVisible(window, timeoutMs = DEFAULT_TIMEOUT_MS) {
  if (!window || typeof window.on !== 'function' || typeof window.removeListener !== 'function'
      || !window.webContents || typeof window.webContents.on !== 'function'
      || typeof window.webContents.removeListener !== 'function'
      || typeof window.isVisible !== 'function' || typeof window.isDestroyed !== 'function'
      || !Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
    return Promise.reject(new Error('normal Main window readiness inputs are invalid'));
  }

  return new Promise((resolve, reject) => {
    let readyToShow = false;
    let rendererLoaded = false;
    let showEventSeen = false;
    let settled = false;
    let visibilityCheck = null;

    const snapshot = () => {
      let destroyed = false;
      let visible = false;
      try { destroyed = window.isDestroyed(); } catch { destroyed = true; }
      if (!destroyed) {
        try { visible = window.isVisible(); } catch { visible = false; }
      }
      return {
        ready_to_show: readyToShow,
        renderer_loaded: rendererLoaded,
        show_event_seen: showEventSeen,
        visible,
        destroyed,
      };
    };

    const cleanup = () => {
      clearTimeout(timer);
      if (visibilityCheck !== null) clearTimeout(visibilityCheck);
      visibilityCheck = null;
      window.removeListener('ready-to-show', onReadyToShow);
      window.removeListener('show', onShow);
      window.removeListener('closed', onClosed);
      window.webContents.removeListener('did-finish-load', onRendererLoaded);
    };
    const finish = (error = null) => {
      if (settled) return;
      settled = true;
      if (error) error.diagnostics = snapshot();
      cleanup();
      if (error) reject(error);
      else resolve();
    };
    const queueVisibilityCheck = () => {
      if (settled || visibilityCheck !== null || !readyToShow || !rendererLoaded) return;
      // Electron can emit `show` before isVisible() reflects the new state. Recheck
      // with a bounded poll so a missing visibility event does not spin the Main
      // thread. Readiness events still check immediately; the deadline is unchanged.
      visibilityCheck = setTimeout(() => {
        visibilityCheck = null;
        maybeResolve();
      }, 25);
    };
    const maybeResolve = () => {
      const state = snapshot();
      if (state.destroyed) return finish(new Error('normal Main window was destroyed before it became visible'));
      if (readyToShow && rendererLoaded && state.visible) return finish();
      queueVisibilityCheck();
    };
    const onReadyToShow = () => { readyToShow = true; maybeResolve(); };
    const onRendererLoaded = () => { rendererLoaded = true; maybeResolve(); };
    const onShow = () => { showEventSeen = true; maybeResolve(); };
    const onClosed = () => finish(new Error('normal Main window closed before it became visible'));
    const timer = setTimeout(() => {
      finish(new Error('normal Main window did not become visible through its ready-to-show path'));
    }, timeoutMs);

    window.on('ready-to-show', onReadyToShow);
    window.on('show', onShow);
    window.on('closed', onClosed);
    window.webContents.on('did-finish-load', onRendererLoaded);
    maybeResolve();
  });
}

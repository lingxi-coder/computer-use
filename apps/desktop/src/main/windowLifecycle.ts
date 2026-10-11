export interface RetainedWindow {
  hide(): void;
  on(event: 'close', listener: (event: { preventDefault(): void }) => void): unknown;
}

/** Closing the console keeps its renderer and the independent stop controls alive. */
export function hideWindowOnClose(window: RetainedWindow, isQuitting: () => boolean): void {
  window.on('close', (event) => {
    if (isQuitting()) return;
    event.preventDefault();
    window.hide();
  });
}

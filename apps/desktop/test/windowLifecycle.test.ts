import assert from 'node:assert/strict';
import { test } from 'node:test';

import { hideWindowOnClose, type RetainedWindow } from '../src/main/windowLifecycle.js';

class Window implements RetainedWindow {
  hidden = false;
  destroyed = false;
  private closing: ((event: { preventDefault(): void }) => void) | null = null;

  on(_event: 'close', listener: (event: { preventDefault(): void }) => void): void {
    this.closing = listener;
  }

  hide(): void { this.hidden = true; }

  close(): void {
    let prevented = false;
    this.closing?.({ preventDefault: () => { prevented = true; } });
    if (!prevented) this.destroyed = true;
  }

  show(): void {
    if (this.destroyed) throw new Error('Object has been destroyed');
    this.hidden = false;
  }

  send(): void {
    if (this.destroyed) throw new Error('Object has been destroyed');
  }
}

test('closing the console hides it, preserves projection delivery and allows it to reopen', () => {
  const window = new Window();
  hideWindowOnClose(window, () => false);
  window.close();
  assert.equal(window.hidden, true);
  assert.equal(window.destroyed, false);
  assert.doesNotThrow(() => window.send());
  assert.doesNotThrow(() => window.show());
  assert.equal(window.hidden, false);
  window.close();
  assert.equal(window.destroyed, false, 'repeated closes still retain the window');
});

test('quitting allows the retained window to close normally', () => {
  let quitting = false;
  const window = new Window();
  hideWindowOnClose(window, () => quitting);
  window.close();
  quitting = true;
  window.close();
  assert.equal(window.destroyed, true);
});

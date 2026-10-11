import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { waitForNormalMainWindowVisible } from './normal-main-window-gate.mjs';

class MockWindow extends EventEmitter {
  visible = false;
  destroyed = false;
  webContents = new EventEmitter();
  isVisible() { return this.visible; }
  isDestroyed() { return this.destroyed; }
}

test('waits for renderer load, Main ready-to-show and actual visible state', async () => {
  const window = new MockWindow();
  const ready = waitForNormalMainWindowVisible(window);
  window.emit('ready-to-show');
  window.webContents.emit('did-finish-load');
  await Promise.resolve();
  window.emit('show');
  assert.equal(window.visible, false, 'a show event alone is not proof of visible state');
  setImmediate(() => { window.visible = true; });
  await ready;
  assert.equal(window.listenerCount('show'), 0, 'readiness listeners are removed after success');
  assert.equal(window.webContents.listenerCount('did-finish-load'), 0);
});

test('checks visibility after Main show handler runs earlier in the ready-to-show event', async () => {
  const window = new MockWindow();
  // Match Main's listener ordering: its show request may happen before the gate's
  // ready-to-show listener has recorded readiness, with visible state set next turn.
  window.once('ready-to-show', () => {
    window.emit('show');
    setImmediate(() => { window.visible = true; });
  });
  const ready = waitForNormalMainWindowVisible(window);
  window.webContents.emit('did-finish-load');
  window.emit('ready-to-show');
  await ready;
});

test('requires Main ready-to-show even if renderer is loaded and window is already visible', async () => {
  const window = new MockWindow();
  window.visible = true;
  let settled = false;
  const ready = waitForNormalMainWindowVisible(window).then(() => { settled = true; });
  window.webContents.emit('did-finish-load');
  window.emit('show');
  await Promise.resolve();
  assert.equal(settled, false);
  window.emit('ready-to-show');
  await ready;
  assert.equal(settled, true);
});

test('observes a visibility state change with no follow-up Electron event', async () => {
  const window = new MockWindow();
  const ready = waitForNormalMainWindowVisible(window, 500);
  window.webContents.emit('did-finish-load');
  window.emit('ready-to-show');
  setTimeout(() => { window.visible = true; }, 10);
  await ready;
});

test('fails closed with bounded readiness diagnostics if Main closes', async () => {
  const window = new MockWindow();
  const ready = waitForNormalMainWindowVisible(window);
  window.destroyed = true;
  window.emit('closed');
  await assert.rejects(ready, error => {
    assert.match(error.message, /closed before it became visible/);
    assert.deepEqual(error.diagnostics, {
      ready_to_show: false,
      renderer_loaded: false,
      show_event_seen: false,
      visible: false,
      destroyed: true,
    });
    return true;
  });
});

test('reports timeout readiness state without extending the production deadline', async () => {
  const window = new MockWindow();
  const ready = waitForNormalMainWindowVisible(window, 5);
  window.webContents.emit('did-finish-load');
  window.emit('ready-to-show');
  window.emit('show');
  await assert.rejects(ready, error => {
    assert.match(error.message, /did not become visible/);
    assert.deepEqual(error.diagnostics, {
      ready_to_show: true,
      renderer_loaded: true,
      show_event_seen: true,
      visible: false,
      destroyed: false,
    });
    return true;
  });
});

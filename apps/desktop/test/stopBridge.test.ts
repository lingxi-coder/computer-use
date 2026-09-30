import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';

import type { BridgeStopStatus, OperatorCommand, StopAck, StopDisposition, StopScope, StopSource } from '@computer-user/desktop-api';

import { DEFAULT_ACCELERATOR, StopBridge, type HotkeyRegistry, type StopTransport } from '../src/main/stopBridge.js';

class FakeTransport extends EventEmitter implements StopTransport {
  calls: string[] = [];
  stops: { scope: StopScope; disposition: StopDisposition; source: StopSource }[] = [];
  commands: OperatorCommand[] = [];

  stopAtDevice(scope: StopScope, disposition: StopDisposition, source: StopSource): void {
    this.calls.push('device');
    this.stops.push({ scope, disposition, source });
  }

  operator(command: OperatorCommand): void {
    this.calls.push('host');
    this.commands.push(command);
  }

  ack(source: StopSource, cleanup: StopAck['cleanup'] = 'not_needed', disposition: StopDisposition = 'cancel'): void {
    this.emit('stopAck', {
      stop_seq: 1,
      stop_floor: 1,
      scope: { scope: 'all' },
      disposition,
      source,
      revoked_permits: 0,
      cleanup,
      at_ms: 10,
    } satisfies StopAck);
  }
}

class FakeHotkeys implements HotkeyRegistry {
  handlers = new Map<string, () => void>();
  refuse = false;

  register(accelerator: string, callback: () => void): boolean {
    if (this.refuse) return false;
    this.handlers.set(accelerator, callback);
    return true;
  }

  unregister(accelerator: string): void {
    this.handlers.delete(accelerator);
  }
}

const statuses = (bridge: StopBridge): BridgeStopStatus[] => {
  const seen: BridgeStopStatus[] = [];
  bridge.on('status', (status) => seen.push(status));
  return seen;
};

test('every entry sends the stop to the device first and then tells the host', () => {
  for (const entry of ['operator_button', 'tray', 'global_hotkey'] as const) {
    const transport = new FakeTransport();
    const bridge = new StopBridge(transport, new FakeHotkeys());
    bridge.press(entry);
    assert.deepEqual(transport.calls, ['device', 'host'], entry);
    assert.deepEqual(transport.stops[0], { scope: { scope: 'all' }, disposition: 'cancel', source: entry });
    assert.deepEqual(transport.commands[0], { command: 'stop_task', root_task_id: null });
    bridge.dispose();
  }
});

test('the hotkey is registered, reaches the same path, and is released on dispose', () => {
  const transport = new FakeTransport();
  const hotkeys = new FakeHotkeys();
  const bridge = new StopBridge(transport, hotkeys);
  assert.equal(bridge.start(), true);
  hotkeys.handlers.get(DEFAULT_ACCELERATOR)?.();
  assert.equal(transport.stops[0]?.source, 'global_hotkey');
  bridge.dispose();
  assert.equal(hotkeys.handlers.size, 0);
});

test('a hotkey the system refuses is reported and does not stop the button from working', () => {
  const transport = new FakeTransport();
  const hotkeys = new FakeHotkeys();
  hotkeys.refuse = true;
  const bridge = new StopBridge(transport, hotkeys);
  assert.equal(bridge.start(), false);
  bridge.press('operator_button');
  assert.equal(transport.stops.length, 1);
  bridge.dispose();
});

test('a click is not a stop: it says stopping until the device confirms it let go', () => {
  const transport = new FakeTransport();
  const bridge = new StopBridge(transport, new FakeHotkeys());
  bridge.start();
  const seen = statuses(bridge);
  bridge.press('tray');
  assert.equal(bridge.status, 'stopping');
  // An acknowledgement for some other entry, or for a pause, is not ours.
  transport.ack('global_hotkey');
  transport.ack('tray', 'not_needed', 'pause');
  assert.equal(bridge.status, 'stopping');
  transport.ack('tray', 'released');
  assert.equal(bridge.status, 'confirmed');
  assert.deepEqual(seen, ['stopping', 'confirmed']);
  bridge.dispose();
});

test('a stop whose release the device could not confirm is shown as unconfirmed', () => {
  for (const cleanup of ['failed', 'unknown', 'held'] as const) {
    const transport = new FakeTransport();
    const bridge = new StopBridge(transport, new FakeHotkeys());
    bridge.start();
    bridge.press('operator_button');
    transport.ack('operator_button', cleanup);
    assert.equal(bridge.status, 'unconfirmed', cleanup);
    bridge.dispose();
  }
});

test('a device that does not answer leaves the stop unconfirmed after the wait', async () => {
  const transport = new FakeTransport();
  const bridge = new StopBridge(transport, new FakeHotkeys(), { confirmTimeoutMs: 20 });
  bridge.start();
  bridge.press('global_hotkey');
  assert.equal(bridge.status, 'stopping');
  await new Promise((done) => setTimeout(done, 60));
  assert.equal(bridge.status, 'unconfirmed');
  // A late confirmation still settles it.
  transport.ack('global_hotkey', 'not_needed');
  assert.equal(bridge.status, 'confirmed');
  bridge.dispose();
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { OperatorCommand } from '@computer-user/desktop-api';

import { HostBridge } from '../src/main/hostBridge.js';
import { isTrustedSender, registerIpc, type IpcEventLike, type TrustedOrigins } from '../src/main/ipc.js';
import type { StopBridge } from '../src/main/stopBridge.js';
import { Channels, INVOKE_CHANNELS } from '../src/shared/channels.js';

const trusted: TrustedOrigins = { files: ['/app/out/renderer/index.html'], devOrigin: 'http://localhost:5173' };
const good: IpcEventLike = { senderFrame: { url: 'file:///app/out/renderer/index.html' } };

function setup() {
  const commands: OperatorCommand[] = [];
  let stops = 0;
  const bridge = new HostBridge(
    { operator: (command) => commands.push(command), projection: null },
    { press: () => (stops += 1) } as unknown as StopBridge,
  );
  const handlers = new Map<string, (event: IpcEventLike, ...args: unknown[]) => unknown>();
  const dispose = registerIpc(
    {
      handle: (channel, listener) => handlers.set(channel, listener),
      removeHandler: (channel) => handlers.delete(channel),
    },
    bridge,
    trusted,
  );
  const call = (channel: string, event: IpcEventLike, ...args: unknown[]): unknown => handlers.get(channel)?.(event, ...args);
  return { commands, call, handlers, dispose, stops: () => stops };
}

test('only the packaged page or the loopback dev server may call main', () => {
  assert.equal(isTrustedSender('file:///app/out/renderer/index.html', trusted), true);
  assert.equal(isTrustedSender('file:///app/out/renderer/index.html#/task', trusted), true);
  assert.equal(isTrustedSender('http://localhost:5173/', trusted), true);
  for (const bad of [
    undefined,
    null,
    '',
    'not a url',
    'file:///tmp/evil.html',
    'http://localhost:9999/',
    'https://example.com/',
    'file:///app/out/renderer/../../evil.html',
  ]) {
    assert.equal(isTrustedSender(bad, trusted), false, String(bad));
  }
  assert.equal(isTrustedSender('http://localhost:5173/', { files: [] }), false, 'no dev origin, no dev access');
});

test('exactly the listed channels are registered', () => {
  const { handlers } = setup();
  assert.deepEqual([...handlers.keys()].sort(), [...INVOKE_CHANNELS].sort());
  // What main sends to the renderer is not something the renderer can invoke.
  assert.equal(handlers.has(Channels.projection), false);
  assert.equal(handlers.has(Channels.stopStatus), false);
});

test('a call from an untrusted page is refused before anything happens', () => {
  const { call, commands, stops } = setup();
  const evil: IpcEventLike = { senderFrame: { url: 'https://example.com/' } };
  for (const channel of INVOKE_CHANNELS) {
    assert.throws(() => call(channel, evil, 'x', true), /untrusted sender/, channel);
    assert.throws(() => call(channel, { senderFrame: null }, 'x', true), /untrusted sender/, channel);
  }
  assert.deepEqual(commands, []);
  assert.equal(stops(), 0);
});

test('a valid call becomes exactly the operator command it names', () => {
  const { call, commands, stops } = setup();
  call(Channels.createTask, good, 'turn off the daily picks');
  call(Channels.confirm, good, 'conf-1', true);
  call(Channels.pause, good, 'root-1');
  call(Channels.resume, good, 'root-1');
  call(Channels.setMode, good, 'execute');
  call(Channels.stop, good);
  assert.deepEqual(commands, [
    { command: 'create_task', text: 'turn off the daily picks' },
    { command: 'confirm_goal', confirmation_id: 'conf-1', approved: true },
    { command: 'pause_task', root_task_id: 'root-1' },
    { command: 'resume_task', root_task_id: 'root-1' },
    { command: 'set_mode', mode: 'execute' },
  ]);
  assert.equal(stops(), 1, 'the stop goes through the stop bridge');
  assert.equal(call(Channels.snapshot, good), null);
});

test('arguments that are not what the schema allows are refused, and nothing is sent', () => {
  const { call, commands } = setup();
  const refused: [string, unknown[]][] = [
    [Channels.createTask, ['']],
    [Channels.createTask, ['   ']],
    [Channels.createTask, [42]],
    [Channels.createTask, ['x'.repeat(2_001)]],
    [Channels.confirm, ['conf-1', 'yes']],
    [Channels.confirm, [7, true]],
    [Channels.confirm, ['', true]],
    [Channels.pause, [null]],
    [Channels.resume, [{ id: 'root-1' }]],
    [Channels.setMode, ['root']],
    [Channels.setMode, [undefined]],
  ];
  for (const [channel, args] of refused) {
    assert.throws(() => call(channel, good, ...args), /refused/, `${channel} ${JSON.stringify(args)}`);
  }
  assert.deepEqual(commands, []);
});

test('disposing removes every handler', () => {
  const { handlers, dispose } = setup();
  assert.ok(handlers.size > 0);
  dispose();
  assert.equal(handlers.size, 0);
});

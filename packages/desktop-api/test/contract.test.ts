import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  assertEnvelope,
  encodeFrame,
  EnvelopeFactory,
  FrameDecoder,
  schemaSha256,
  validateEnvelope,
  validateOperatorCommand,
  type Envelope,
} from '../src/index.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

interface Fixture {
  name: string;
  schema_valid: boolean;
  decode_valid: boolean;
  message: unknown;
}

const fixtures: Fixture[] = JSON.parse(readFileSync(resolve(root, 'contracts/desktop-v1/fixtures.json'), 'utf8'));

test('the TypeScript validator agrees with the Rust verdict on every shared fixture', () => {
  assert.ok(fixtures.length >= 60, 'the fixtures are the shared contract');
  for (const fixture of fixtures) {
    const result = validateEnvelope(fixture.message);
    assert.equal(
      result.ok,
      fixture.schema_valid,
      `${fixture.name}: Rust says schema_valid=${fixture.schema_valid}, Ajv says ${result.ok}`,
    );
  }
  // Structurally invalid ones are rejected here; the semantic ones pass the schema
  // and are refused by the Rust decoder, which is the authority on those rules.
  const semantic = fixtures.filter((f) => f.schema_valid && !f.decode_valid);
  assert.ok(semantic.length > 0, 'the fixtures include rules a schema cannot express');
});

test('every valid fixture survives a round trip through the frame codec unchanged', () => {
  for (const fixture of fixtures.filter((f) => f.schema_valid && f.decode_valid)) {
    const decoder = new FrameDecoder();
    decoder.push(encodeFrame(fixture.message as Envelope));
    assert.deepEqual(decoder.next(), fixture.message, fixture.name);
    assert.equal(decoder.next(), null);
  }
});

test('the fixtures cover every message type', () => {
  const types = new Set(
    fixtures
      .filter((f) => f.schema_valid && f.decode_valid)
      .map((f) => (f.message as Envelope).message.type),
  );
  assert.equal(types.size, 36, [...types].sort().join(', '));
});

test('the generated types are the ones the published schema produces', () => {
  execFileSync('node', ['scripts/gen-types.mjs', '--check'], { cwd: root, stdio: 'pipe' });
  const digest = createHash('sha256')
    .update(readFileSync(resolve(root, 'contracts/desktop-v1/schema.json')))
    .digest('hex');
  assert.equal(schemaSha256, digest, 'the generated module names the schema it came from');
});

test('what the renderer may ask of main is a closed set', () => {
  assert.ok(validateOperatorCommand({ command: 'create_task', text: 'turn it off' }).ok);
  assert.ok(validateOperatorCommand({ command: 'stop_task', root_task_id: null }).ok);
  assert.ok(validateOperatorCommand({ command: 'set_mode', mode: 'execute' }).ok);
  for (const bad of [
    { command: 'run_shell', text: 'rm -rf /' },
    { command: 'create_task', text: 'x', execute: true },
    { command: 'confirm_goal', approved: true },
    { command: 'set_mode', mode: 'root' },
    { command: 'pause_task', root_task_id: '' },
    'create_task',
    null,
  ]) {
    assert.equal(validateOperatorCommand(bad).ok, false, JSON.stringify(bad));
  }
});

test('an envelope built here is one the schema accepts, and a broken one is refused with reasons', () => {
  const factory = new EnvelopeFactory('supervisor', () => 1234.9);
  const first = factory.make({ type: 'operator_command', payload: { command: 'create_task', text: 'x' } });
  const second = factory.make({ type: 'shutdown', payload: { reason: 'done' } }, first.message_id);
  assert.equal(first.producer_sequence, 1);
  assert.equal(second.producer_sequence, 2);
  assert.equal(second.in_reply_to, first.message_id);
  assert.equal(first.emitted_ms, 1234);
  assertEnvelope(first);
  assertEnvelope(second);

  const broken = { ...first, protocol_version: 2 };
  const result = validateEnvelope(broken);
  assert.equal(result.ok, false);
  if (!result.ok) assert.ok(result.errors.length > 0);
  assert.throws(() => assertEnvelope(broken), /not a desktop v1 envelope/);
});

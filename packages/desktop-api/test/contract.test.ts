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
  PROTOCOL_VERSION,
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

const fixtures: Fixture[] = JSON.parse(readFileSync(resolve(root, 'contracts/desktop-v2/fixtures.json'), 'utf8'));

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

test('desktop v2 does not decode legacy untagged observation, intent, or receipt payloads', () => {
  const legacyFixtures: Fixture[] = JSON.parse(readFileSync(resolve(root, 'contracts/desktop-v1/fixtures.json'), 'utf8'));
  for (const name of ['valid_observation', 'valid_submit_intent', 'valid_receipt']) {
    const fixture = legacyFixtures.find((candidate) => candidate.name === name);
    assert.ok(fixture?.decode_valid, `the historical v1 ${name} fixture exists`);
    const legacy = JSON.parse(JSON.stringify(fixture.message)) as Record<string, unknown>;
    legacy.protocol_version = PROTOCOL_VERSION;
    assert.equal(validateEnvelope(legacy).ok, false, `v2 rejects the former ${name} wire shape`);
  }
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
  assert.equal(types.size, 37, [...types].sort().join(', '));
});

function structureSnapshotWithTableViewport(): {
  envelope: Record<string, unknown>;
  table: Record<string, unknown>;
  row: Record<string, unknown>;
} {
  const fixture = fixtures.find((entry) => entry.name === 'valid_structure_snapshot');
  assert.ok(fixture, 'the shared structure fixture exists');
  const envelope = JSON.parse(JSON.stringify(fixture.message)) as Record<string, unknown>;
  const message = envelope['message'] as Record<string, unknown>;
  const payload = message['payload'] as Record<string, unknown>;
  const frame = { x: 0, y: 0, width: 80, height: 24 };
  const row: Record<string, unknown> = {
    parent_instance_ref: 'table-1',
    frame,
    ax_enabled: 'enabled',
    cells_complete: true,
    cells: [],
  };
  const table: Record<string, unknown> = {
    table_instance_ref: 'table-1',
    scroll_container_instance_ref: 'scroll-1',
    process_id: 4242,
    ax_enabled: 'enabled',
    table_frame: frame,
    viewport_frame: frame,
    visible_rows_complete: true,
    visible_rows: [row],
    columns_complete: true,
    columns: [],
  };
  payload['table_viewports'] = [table];
  return { envelope, table, row };
}

test('scoped table census has a strict wire shape independent of global completeness', () => {
  const { envelope } = structureSnapshotWithTableViewport();
  const message = envelope['message'] as Record<string, unknown>;
  const payload = message['payload'] as Record<string, unknown>;
  payload['complete'] = false;
  payload['incomplete_reason'] = 'AX node budget exceeded';
  const census: Record<string, unknown> = {
    requested_scope: {
      scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
      column_headers: ['Name'],
    },
    process_id: 4242,
    window_id: 'window-1',
    state: { state: 'complete' },
    candidates: [],
  };
  payload['table_scope_search'] = census;
  assert.equal(validateEnvelope(envelope).ok, true);
  census['state'] = { state: 'incomplete', reason: 'scope_time_budget_exceeded' };
  assert.equal(validateEnvelope(envelope).ok, true);
  census['state'] = { state: 'incomplete' };
  assert.equal(validateEnvelope(envelope).ok, false);
  census['state'] = { state: 'complete' };
  const request = census['requested_scope'] as Record<string, unknown>;
  request['row_index'] = 101;
  assert.equal(validateEnvelope(envelope).ok, false);
  delete request['row_index'];
  census['process_id'] = 0;
  assert.equal(validateEnvelope(envelope).ok, false);
  census['process_id'] = 4242;
  delete census['candidates'];
  assert.equal(validateEnvelope(envelope).ok, false);
  // Cross-read object identity and enumeration completeness are semantic
  // checks made by Rust and Native Source, beyond JSON Schema validation.
});

test('table viewport and actual row require explicit AXEnabled observations', () => {
  assert.equal(validateEnvelope(structureSnapshotWithTableViewport().envelope).ok, true);

  const missingTableObservation = structureSnapshotWithTableViewport();
  delete missingTableObservation.table['ax_enabled'];
  assert.equal(validateEnvelope(missingTableObservation.envelope).ok, false);

  const missingRowObservation = structureSnapshotWithTableViewport();
  delete missingRowObservation.row['ax_enabled'];
  assert.equal(validateEnvelope(missingRowObservation.envelope).ok, false);

  const genericEnabledRemainsBoolean = structureSnapshotWithTableViewport();
  const message = genericEnabledRemainsBoolean.envelope['message'] as Record<string, unknown>;
  const payload = message['payload'] as Record<string, unknown>;
  const objects = payload['objects'] as Record<string, unknown>[];
  objects[0]!['enabled'] = false;
  assert.equal(validateEnvelope(genericEnabledRemainsBoolean.envelope).ok, true);
  objects[0]!['enabled'] = 'unavailable';
  assert.equal(validateEnvelope(genericEnabledRemainsBoolean.envelope).ok, false);
});

test('table and row AXEnabled observations reject unknown wire strings', () => {
  const validStates = ['enabled', 'disabled', 'attribute_unsupported', 'unavailable'];
  for (const state of validStates) {
    const { envelope, table, row } = structureSnapshotWithTableViewport();
    table['ax_enabled'] = state;
    row['ax_enabled'] = state;
    assert.equal(validateEnvelope(envelope).ok, true, `recognized AXEnabled state ${state}`);
  }

  for (const state of ['unknown', 'unsupported', 'not_reported']) {
    const invalidTable = structureSnapshotWithTableViewport();
    invalidTable.table['ax_enabled'] = state;
    assert.equal(validateEnvelope(invalidTable.envelope).ok, false, `unknown table state ${state}`);

    const invalidRow = structureSnapshotWithTableViewport();
    invalidRow.row['ax_enabled'] = state;
    assert.equal(validateEnvelope(invalidRow.envelope).ok, false, `unknown row state ${state}`);
  }
});

test('the generated types are the ones the published schema produces', () => {
  execFileSync('node', ['scripts/gen-types.mjs', '--check'], { cwd: root, stdio: 'pipe' });
  const digest = createHash('sha256')
    .update(readFileSync(resolve(root, 'contracts/desktop-v2/schema.json')))
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
  assert.equal(first.protocol_version, PROTOCOL_VERSION);
  assert.equal(second.in_reply_to, first.message_id);
  assert.equal(first.emitted_ms, 1234);
  assertEnvelope(first);
  assertEnvelope(second);

  const broken = { ...first, protocol_version: 1 };
  const result = validateEnvelope(broken);
  assert.equal(result.ok, false);
  if (!result.ok) assert.ok(result.errors.length > 0);
  assert.throws(() => assertEnvelope(broken), /not a desktop v2 envelope/);
});

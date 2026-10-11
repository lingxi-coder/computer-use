import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import {
  validateBrowserControlAuthority,
  validateBrowserControlBootstrap,
  validateBrowserControlHello,
  validateBrowserControlOutcome,
  validateBrowserControlReady,
  validateBrowserControlReply,
  validateBrowserControlSubmit,
  type BrowserControlBootstrap,
} from '../src/index.js';

interface ControlGoldens {
  bootstrap: unknown;
  hello: unknown;
  ready: unknown;
  submit: unknown;
  reply: unknown;
}

const fixturePath = resolve(dirname(fileURLToPath(import.meta.url)), '../../../contracts/browser-v1/control-goldens.json');
const goldens = JSON.parse(readFileSync(fixturePath, 'utf8')) as ControlGoldens;

test('Rust-generated private browser-control DTOs pass strict TypeScript parity validators', async () => {
  const bootstrap = validateBrowserControlBootstrap(goldens.bootstrap);
  assert.equal(bootstrap.ok, true);
  if (!bootstrap.ok) return;

  const hello = validateBrowserControlHello(goldens.hello, bootstrap.value);
  assert.equal(hello.ok, true);
  if (!hello.ok) return;
  assert.equal(validateBrowserControlReady(goldens.ready, hello.value).ok, true);

  const submit = await validateBrowserControlSubmit(goldens.submit);
  assert.equal(submit.ok, true);
  assert.equal(validateBrowserControlAuthority((goldens.submit as { authority: unknown }).authority).ok, true);
  assert.equal(validateBrowserControlOutcome('completed').ok, true);
  assert.equal(validateBrowserControlOutcome('unknown').ok, true);
  assert.equal(validateBrowserControlOutcome('not_completed').ok, false);
  assert.equal(await validateBrowserControlReply(goldens.reply, goldens.submit).then((result) => result.ok), true);
});

test('bootstrap credentials are strict, bound to the installed scope, and never echoed in errors', () => {
  const secret = '11'.repeat(32);
  const changedBootstrap = structuredClone(goldens.bootstrap) as BrowserControlBootstrap & Record<string, unknown>;
  changedBootstrap.token = '22'.repeat(32);
  const mismatch = validateBrowserControlHello(goldens.hello, changedBootstrap);
  assert.equal(mismatch.ok, false);
  if (!mismatch.ok) assert.equal(mismatch.errors.join(' ').includes(secret), false);

  const unknown = structuredClone(goldens.bootstrap) as Record<string, unknown>;
  unknown['renderer_selected_profile'] = { kind: 'arbitrary' };
  assert.equal(validateBrowserControlBootstrap(unknown).ok, false);

  const byteOverflow = structuredClone(goldens.bootstrap) as BrowserControlBootstrap;
  byteOverflow.scope.context.frame_id = '界'.repeat(86);
  assert.equal(validateBrowserControlBootstrap(byteOverflow).ok, false,
    'identity limits count UTF-8 bytes like serde, not JavaScript code units');

  const malformed = structuredClone(goldens.bootstrap) as BrowserControlBootstrap;
  malformed.token = 'AF'.repeat(32);
  const invalid = validateBrowserControlBootstrap(malformed);
  assert.equal(invalid.ok, false);
  if (!invalid.ok) {
    assert.equal(invalid.errors.join(' ').includes(secret), false);
    assert.equal(invalid.errors.join(' ').includes(malformed.token), false);
  }
});

test('submit preserves and validates authority evidence independently from the newer intent evidence', async () => {
  const valid = structuredClone(goldens.submit) as Record<string, any>;
  const checked = await validateBrowserControlSubmit(valid);
  assert.equal(checked.ok, true);
  assert.notEqual(valid.authority_evidence.observation_id, valid.intent.evidence.observation_id,
    'the Rust fixture keeps permit evidence distinct from the newer execution read');

  const overlap = structuredClone(valid);
  overlap.intent.evidence.binding.observed_ms = overlap.authority_evidence.binding.read_finished_ms - 1;
  assert.equal((await validateBrowserControlSubmit(overlap)).ok, false,
    'the new read cannot overlap the read used for original permit authority');

  const mixedAuthority = structuredClone(valid);
  mixedAuthority.authority_evidence.binding.ax_node_id = 'another-ax-node';
  assert.equal((await validateBrowserControlSubmit(mixedAuthority)).ok, false,
    'authority evidence must continue to name the exact current AX target');

  const missingAuthority = structuredClone(valid);
  delete missingAuthority.authority_evidence;
  assert.equal((await validateBrowserControlSubmit(missingAuthority)).ok, false,
    'the original authority evidence is an independent required submit field');
});

test('reply status and correlation rules reject replay, missing completion bounds, and extra properties', async () => {
  const incomplete = structuredClone(goldens.reply) as Record<string, any>;
  incomplete.input_end_ms = null;
  assert.equal((await validateBrowserControlReply(incomplete, goldens.submit)).ok, false);

  const replayed = structuredClone(goldens.reply) as Record<string, any>;
  replayed.sequence += 1;
  assert.equal((await validateBrowserControlReply(replayed, goldens.submit)).ok, false);

  const extra = structuredClone(goldens.reply) as Record<string, unknown>;
  extra['debug_token'] = '11'.repeat(32);
  const result = await validateBrowserControlReply(extra, goldens.submit);
  assert.equal(result.ok, false);
  if (!result.ok) assert.equal(result.errors.join(' ').includes('11'.repeat(32)), false);
});

#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler, nativeFixtureAuthorityContext } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Serial owned-fixture native-helper acceptance. Never builds shared binaries.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const fixtureBundle = 'com.lingxi.computer-user.guard-fixture';
const itemLabel = 'Fixture menu first item';
const scenarios = ['applied', 'stale-deadline', 'closed-context', 'source-session-restart',
  'anchor-replacement', 'changed-ceiling', 'foreign-geometry', 'same-pid-other-window',
  'disabled-item', 'duplicate-label', 'unknown-selection'];
const controls = { 'anchor-replacement': 'replace-opener', 'foreign-geometry': 'move-origin-window',
  'disabled-item': 'disable-first', 'duplicate-label': 'duplicate-items', 'unknown-selection': 'delay-selection' };
const options = parseArgs({ options: {
  run: { type: 'boolean', default: false }, 'self-test': { type: 'boolean', default: false },
  'list-scenarios': { type: 'boolean', default: false }, fixture: { type: 'string' },
  source: { type: 'string', default: join(root, 'target/debug/cu-macos-source') },
  helper: { type: 'string', default: join(root, 'target/debug/cu-macos-device-guard') },
  scenarios: { type: 'string', default: scenarios.join(',') }, output: { type: 'string' },
} }).values;

const normalize = text => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = value => Math.sign(value) * Math.floor(Math.abs(value) + 0.5);
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const digest = fp => 'fp1:' + createHash('sha256').update(JSON.stringify(fp)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const exactFrame = (a, b) => ['x', 'y', 'width', 'height'].every(field => a?.[field] === b?.[field]);

function createSourceAssets(path, createDirectory = mkdirSync) {
  createDirectory(path, { mode: 0o700 });
}
const runMetadata = (ok, artifact, output) => ({ ok, artifact, output, mvp_complete: false });

// Representation only: missing nullable fp fields decode to None in Rust and
// serialize as null. No source identity, geometry or provenance is replaced.
function canonicalFingerprint(fp) {
  return { app: fp.app, app_pid: fp.app_pid, window_id: fp.window_id,
    ancestor_roles: [...fp.ancestor_roles], group: fp.group ?? null, role: fp.role,
    subrole: fp.subrole ?? null, label: fp.label, sibling_index: fp.sibling_index,
    frame: [...fp.frame], scroll_container: fp.scroll_container ?? null };
}
function sourceFingerprint(structure, object) {
  const ancestry = [], seen = new Set(); let parentID = object.parent;
  while (parentID) {
    assert.ok(!seen.has(parentID) && ancestry.length < 32, 'bounded real source ancestry'); seen.add(parentID);
    const parent = structure.objects.find(value => value.instance_ref === parentID); assert.ok(parent);
    ancestry.push(parent.role); parentID = parent.parent;
  }
  const siblings = structure.objects.filter(value => value.parent === object.parent && value.window_id === object.window_id);
  return canonicalFingerprint({ app: structure.app.bundle_id, app_pid: structure.app.pid, window_id: object.window_id,
    ancestor_roles: ancestry.reverse(), group: object.group == null ? null : normalize(object.group), role: object.role,
    subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: siblings.findIndex(value => value.instance_ref === object.instance_ref),
    frame: ['x', 'y', 'width', 'height'].map(field => round(object.frame[field])) });
}
function canonicalAnchor(anchor) {
  const result = structuredClone(anchor); result.opener_fingerprint = canonicalFingerprint(anchor.opener_fingerprint); return result;
}
function canonicalContext(context) {
  const result = structuredClone(context); result.opener = canonicalAnchor(context.opener);
  result.menu_fingerprint = canonicalFingerprint(context.menu_fingerprint); return result;
}
function menuCandidates(structure, label) {
  const context = structure.menu_context; assert.ok(context, 'actual Source menu context required');
  return structure.objects.filter(object => object.parent === context.menu_instance && object.role === 'AXMenuItem' &&
    object.window_id === context.opener.origin_window_id && normalize(object.title ?? object.description ?? '') === normalize(label));
}
function freshRecords(entry, start) {
  const count = Math.min(entry.records.length, entry.source_records_seen - start);
  return count > 0 ? entry.records.slice(-count) : [];
}
function preservedFileHashes(directory) {
  const result = {};
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) Object.assign(result, preservedFileHashes(path));
    else if (entry.isFile() && entry.name !== 'm3-native-menu-selection.json') result[path] = sha(path);
  }
  return result;
}
function boundProof(record, object, menuScope) {
  assert.ok(Number.isSafeInteger(record.tree_revision) && record.tree_revision > 0, 'actual source tree revision');
  return { binding: 'owned-menu-' + object.role.toLowerCase(), instance: object.instance_ref,
    snapshot_id: `watch-${record.stream_id}-${record.sequence}`, environment_revision: Math.max(1, record.tree_revision),
    layer_revision: 1, fingerprint: sourceFingerprint(record.structure, object), display_frame: structuredClone(object.frame),
    menu_scope: menuScope, ...nativeFixtureBindingEvidence(record.structure, object, record.sequence) };
}
function assertNativeSourceRecord(record, ready) {
  assert.equal(record.kind, 'observation'); assert.equal(record.structure.app.pid, ready.pid);
  assert.equal(record.structure.app.bundle_id, fixtureBundle);
  assert.ok(Number.isSafeInteger(record.sequence) && Number.isSafeInteger(record.structure_finished_unix_ms));
  for (const anchor of [...record.structure.menu_anchors ?? [], ...(record.structure.menu_context ? [record.structure.menu_context.opener] : [])]) {
    assert.equal(anchor.verified_source_sequence, record.sequence, 'native producer owns sequence');
    assert.equal(anchor.verified_observed_ms, record.structure_finished_unix_ms, 'native producer owns UNIX-ms time');
    assert.equal(anchor.process.pid, ready.pid); assert.equal(anchor.origin_window_id, `window-${ready.window_id}`);
    const object = record.structure.objects.find(value => value.instance_ref === anchor.opener_instance); assert.ok(object);
    assert.deepEqual(canonicalFingerprint(anchor.opener_fingerprint), sourceFingerprint(record.structure, object));
    assert.ok(exactFrame(anchor.opener_frame, object.frame));
  }
}

function selfTest() {
  let count = 0;
  const check = condition => { assert.ok(condition); count += 1; };
  check(normalize(' Fixture\u00a0 MENU  first item ') === normalize(itemLabel));
  check(round(-1.5) === -2 && round(1.5) === 2);
  const fp = canonicalFingerprint({ app: fixtureBundle, app_pid: 1, window_id: 'window-1',
    ancestor_roles: ['AXWindow'], role: 'AXMenuButton', label: 'fixture menu', sibling_index: 0, frame: [0, 0, 10, 10] });
  check(fp.group === null && fp.subrole === null && fp.scroll_container === null);
  check(digest(fp).startsWith('fp1:') && digest(fp).length === 68);
  const anchor = { opener_fingerprint: fp, verified_source_sequence: 3, verified_observed_ms: 123 };
  check(JSON.stringify(canonicalAnchor(anchor)) === JSON.stringify(anchor));
  check(anchor.verified_observed_ms === 123 && anchor.verified_source_sequence === 3);
  const structure = { menu_context: { menu_instance: 'menu-1', opener: { origin_window_id: 'window-1' } },
    objects: [{ instance_ref: 'one', parent: 'menu-1', window_id: 'window-1', role: 'AXMenuItem', title: itemLabel },
      { instance_ref: 'foreign', parent: 'menu-2', window_id: 'window-1', role: 'AXMenuItem', title: itemLabel }] };
  check(menuCandidates(structure, itemLabel).length === 1);
  structure.objects.push({ ...structure.objects[0], instance_ref: 'duplicate' });
  check(menuCandidates(structure, itemLabel).length === 2);
  check(exactFrame({ x: 0, y: 1, width: 2, height: 3 }, { x: 0, y: 1, width: 2, height: 3 }));
  check(!exactFrame({ x: 0, y: 1, width: 2, height: 3 }, { x: 0.25, y: 1, width: 2, height: 3 }));
  check(freshRecords({ records: [{ request_id: 'old' }], source_records_seen: 1 }, 1).length === 0);
  check(freshRecords({ records: [{ request_id: 'old' }, { request_id: 'new' }], source_records_seen: 2 }, 1)[0].request_id === 'new');
  let assetsCreation;
  createSourceAssets('/owned/source-assets', (path, options) => { assetsCreation = { path, options }; });
  check(assetsCreation.path === '/owned/source-assets' && assetsCreation.options.mode === 0o700);
  check(JSON.stringify(runMetadata(false, '/owned/report.json', '/owned')) ===
    JSON.stringify({ ok: false, artifact: '/owned/report.json', output: '/owned', mvp_complete: false }));
  console.log(JSON.stringify({ kind: 'script_self_test', passed: count, physical_input_run: false, mvp_complete: false }));
}

async function run() {
  assert.equal(process.platform, 'darwin', 'native owned matrix requires macOS');
  assert.ok(options.fixture, '--fixture must identify the executable of a run-owned MenuFixture.app');
  const binaries = { fixture: resolve(options.fixture), source: resolve(options.source), helper: resolve(options.helper) };
  for (const path of Object.values(binaries)) assert.ok(existsSync(path), 'prebuilt binary required: ' + path);
  const selected = options.scenarios.split(',').filter(Boolean);
  assert.ok(selected.length > 0 && new Set(selected).size === selected.length);
  for (const scenario of selected) assert.ok(scenarios.includes(scenario), 'unknown scenario: ' + scenario);
  const output = options.output ? resolve(options.output) : mkdtempSync(join(tmpdir(), 'cu-owned-menu-selection-'));
  mkdirSync(output, { recursive: true });
  assert.equal(readdirSync(output).length, 0, 'preserve previous failures: use a fresh empty output directory');
  const artifact = join(output, 'm3-native-menu-selection.json');
  const evidence = { ok: false, started_at: new Date().toISOString(), output, physical_input_run: false,
    production_core_native_backend_connected: false, direct_native_helper_boundary_run: true, real_provider_calls: 0,
    mvp_complete: false, menu_pixels_claimed: false, scenarios: [], unselected_scenarios: scenarios.filter(name => !selected.includes(name)),
    build: Object.fromEntries(Object.values(binaries).map(path => [path, sha(path)])),
    source_files: Object.fromEntries(['scripts/smoke-macos-menu-selection.mjs', 'native/device/fixtures/MenuFixture.swift',
      'native/device/MenuRelation.swift', 'native/device/AtomicMenuSelect.swift', 'native/device/Authority.swift',
      'native/macos/MenuContext.swift', 'native/macos/SourceMain.swift'].map(path => [path, sha(join(root, path))])) };
  const save = () => writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n');
  const children = [], timers = new Set(); let activeFixture;
  save();
  function child(binary, args, label) {
    const proc = spawn(binary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
    const entry = { proc, label, records: [], parse_error: null, stderr: '', source_records_seen: 0, closed: false };
    children.push(entry); proc.stdin.on('error', () => {}); proc.on('error', error => { entry.parse_error = String(error); });
    proc.on('close', () => { entry.closed = true; });
    let pending = '';
    proc.stdout.on('data', chunk => {
      appendFileSync(join(output, label + '.stdout.jsonl'), chunk); pending += chunk;
      if (pending.length > 768 * 1024) { entry.parse_error = 'stdout line exceeds producer budget'; return; }
      for (let end; (end = pending.indexOf('\n')) >= 0;) {
        try { const record = JSON.parse(pending.slice(0, end)); entry.records.push(record); entry.source_records_seen += 1; }
        catch (error) { entry.parse_error = String(error); }
        pending = pending.slice(end + 1); if (entry.records.length > 256) entry.records.shift();
      }
    });
    proc.stderr.on('data', chunk => { appendFileSync(join(output, label + '.stderr.txt'), chunk); entry.stderr = (entry.stderr + chunk).slice(-8192); });
    return entry;
  }
  function healthy(entry) {
    assert.equal(entry.parse_error, null, entry.parse_error);
    if (entry.proc.exitCode != null || entry.proc.signalCode != null) throw new Error(`${entry.label} ended: ${entry.proc.exitCode ?? entry.proc.signalCode}; ${entry.stderr}`);
  }
  async function until(read, reason, timeout = 4000) {
    const end = Date.now() + timeout;
    while (Date.now() < end) { const result = await read(); if (result) return result; await pause(10); }
    throw new Error('Timeout: ' + reason);
  }
  async function fixtureCommand(fixture, name) {
    healthy(fixture); const start = fixture.source_records_seen; fixture.proc.stdin.write(name + '\n');
    return until(() => { healthy(fixture); return freshRecords(fixture, start).find(value => value.kind === name); }, name);
  }
  async function endChild(entry) {
    if (entry.closed) return;
    if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    await until(() => entry.closed, entry.label + ' drained close', 4000);
  }
  function startWatch(ready, label) {
    const watch = child(binaries.source, [], label), assets = join(output, label + '-assets'); createSourceAssets(assets);
    const request = { protocol_version: 1, command: 'watch', bundle_id: fixtureBundle, pid: ready.pid, output_dir: assets,
      interval_ms: 250, timeout_ms: 3000, max_nodes: 128, max_depth: 12, clock_origin_unix_ms: 0,
      text_policy: { kind: 'registered_editor', window_id: `window-${ready.window_id}` } };
    writeFileSync(join(output, label + '.request.json'), JSON.stringify(request) + '\n');
    watch.proc.stdin.end(JSON.stringify(request)); return watch;
  }
  async function observation(watch, ready, predicate, reason, timeout = 4000) {
    return until(() => {
      healthy(watch);
      const record = watch.records.findLast(value => value.kind === 'observation' && predicate(value));
      if (record) { assertNativeSourceRecord(record, ready); return record; }
      return false;
    }, reason, timeout);
  }
  function nativeControl(guard, value) {
    healthy(guard); const packet = { protocol_version: 1, session_epoch: 7, ...value };
    const encoded = JSON.stringify(packet); assert.ok(Buffer.byteLength(encoded) <= 8192, 'native control line bound');
    appendFileSync(join(output, guard.label + '.request-timing.jsonl'), JSON.stringify({
      command: packet.command, lane: packet.lane ?? null, duration_ms: packet.duration_ms ?? null,
      request_id: packet.probe?.request_id ?? null, host_send_unix_ms: Date.now(),
      host_send_monotonic_ms: performance.now(), diagnostic_only: true }) + '\n');
    appendFileSync(join(output, guard.label + '.requests.jsonl'), encoded + '\n'); guard.proc.stdin.write(encoded + '\n');
  }
  async function submit(guard, packet, label) {
    const start = guard.source_records_seen; nativeControl(guard, { command: 'atomic_submit', probe: packet });
    const response = await until(() => {
      healthy(guard); const fresh = freshRecords(guard, start);
      return fresh.find(value => value.kind === 'atomic_submit' && value.request_id === packet.request_id);
    }, label + ' actual native receipt', 3000);
    return response;
  }
  async function unexpectedSelectionDiagnostics(guard, receipt) {
    const index = guard.records.indexOf(receipt);
    const start = index >= 0 ? guard.source_records_seen - guard.records.length + index + 1 : guard.source_records_seen;
    const began = performance.now(), cutoff = began + 250;
    const diagnostic = { request_id: receipt.request_id, wait_budget_ms: 250, guard_snapshot: null };
    // Observe a real record after this receipt, including one already parsed in
    // the same stdout chunk. Existing lease renewal remains active; no input
    // request or authority retry is issued by this diagnostic.
    while (performance.now() < cutoff) {
      const snapshot = freshRecords(guard, start).find(value => value.kind === 'guard');
      if (snapshot) { diagnostic.guard_snapshot = snapshot; break; }
      if (guard.closed || guard.parse_error) { diagnostic.guard_error = guard.parse_error ?? 'guard closed'; break; }
      await pause(Math.min(10, Math.max(0, cutoff - performance.now())));
    }
    diagnostic.waited_ms = performance.now() - began;
    return diagnostic;
  }
  function packet(record, binding, action, scope, attempt, ceiling, label, guard) {
    const now = Date.now(), duration = Math.min(1000, ceiling - now); assert.ok(duration > 100, 'original activated-attempt ceiling remains');
    const id = `${label}-${randomUUID()}`;
    const state = guard.records.findLast(value => value.kind === 'guard'); assert.ok(state);
    assert.equal(state.stop_revision, 0, 'no guard reset or stop generation borrowing');
    const proof = boundProof(record, binding, scope);
    return { request_id: id, intent_id: id, task_id: attempt.task_id, task_revision: 1, root_epoch: 1, attempt_id: attempt.id,
      permit: { id: id + '-permit', generation: 1 }, stop_seq: 0, guard_revision: state.stop_revision,
      clock_domain: 'macos/7/wall', surface: { id: 'main', epoch: 7 }, evidence_id: id + '-evidence', intent_evidence_id: id + '-original',
      source_sequence: record.sequence, observed_ms: record.structure_finished_unix_ms, valid_until_ms: now + duration,
      command_end_ms: now + duration, started_ms: now, duration_ms: duration, latest_start_ms: now,
      ...nativeFixtureAuthorityContext(proof), binding_digest: digest(proof.fingerprint),
      dependencies: { focus: 1, layer: 1, binding: 1, authorization: 1, geometry: 1, menu: 1 }, action,
      x: binding.frame.x + binding.frame.width / 2, y: binding.frame.y + binding.frame.height / 2 };
  }
  function rejected(response) { assert.equal(response.submitted, false); assert.notEqual(response.outcome, 'applied'); }
  try {
    for (const scenario of selected) {
      const step = { scenario, status: 'running', category: scenario === 'changed-ceiling' ? 'hostile-wire-negative' : 'actual-fixture-boundary' };
      evidence.scenarios.push(step); save();
      const fixture = child(binaries.fixture, [], scenario + '-fixture'); activeFixture = fixture;
      let watch, guard, timer;
      try {
        const ready = await until(() => { healthy(fixture); return fixture.records.find(value => value.kind === 'ready'); }, 'owned fixture ready');
        assert.equal(ready.pid, fixture.proc.pid); assert.equal(ready.bundle_id, fixtureBundle); step.target = ready;
        if (controls[scenario]) assert.ok(ready.fault_controls?.includes(controls[scenario]), 'actual fixture control unavailable: ' + controls[scenario]);
        await fixtureCommand(fixture, 'accept-menu');
        if (scenario === 'duplicate-label' || scenario === 'unknown-selection') await fixtureCommand(fixture, controls[scenario]);
        await fixtureCommand(fixture, 'focus-window'); step.before = await fixtureCommand(fixture, 'stats');
        assert.equal(step.before.action_calls, 0); assert.equal(step.before.selection_calls, 0);
        guard = child(binaries.helper, ['--epoch', '7', '--marker', '123', '--execute-pid', String(ready.pid),
          '--execute-bundle', fixtureBundle, '--execute-window', `window-${ready.window_id}`], scenario + '-guard');
        nativeControl(guard, { command: 'configure_authority', clock_origin_ms: 0, clock_domain: 'macos/7/wall', surface: { id: 'main', epoch: 7 } });
        const renew = () => {
          if (guard.proc.exitCode == null && guard.proc.signalCode == null) {
            try { for (const lane of ['host', 'supervisor']) nativeControl(guard, { command: 'lease', lane, duration_ms: 1500 }); }
            catch (error) { guard.parse_error ??= String(error); }
          }
        };
        renew(); timer = setInterval(renew, 100); timers.add(timer);
        const state = await until(() => { healthy(guard); return guard.records.findLast(value => value.kind === 'guard' && value.authority_clock_ready && value.host_alive && value.supervisor_alive); }, 'native leases/clock');
        assert.equal(state.accessibility, true); assert.equal(state.human_input, true); assert.equal(state.startup_mouse_buttons_clear, true);
        watch = startWatch(ready, scenario + '-watch');
        const baseline = await observation(watch, ready, value => value.structure.complete && value.structure.active_layer === `window-${ready.window_id}` &&
          !value.structure.menu_context && value.structure.menu_anchors?.length > 0, 'actual stable opener source anchor');
        const anchors = baseline.structure.menu_anchors.filter(value => value.opener_fingerprint.label === normalize(ready.label) && exactFrame(value.opener_frame, ready.frame));
        assert.equal(anchors.length, 1, 'unique actual opener anchor'); const anchor = anchors[0];
        const opener = baseline.structure.objects.find(value => value.instance_ref === anchor.opener_instance); assert.ok(opener?.enabled);
        const attempt = { id: 'attempt-' + randomUUID(), task_id: 'owned-menu-root-' + randomUUID() }, ceiling = Date.now() + 6000;
        const openingScope = { kind: 'opener', anchor: canonicalAnchor(anchor), attempt_deadline_ms: ceiling };
        const opening = packet(baseline, opener, 'ax_show_menu', openingScope, attempt, ceiling, 'open', guard);
        evidence.physical_input_run = true; step.opening_request = opening; save();
        step.opening_receipt = await submit(guard, opening, scenario + ' ShowMenu');
        assert.equal(step.opening_receipt.outcome, 'applied'); assert.equal(step.opening_receipt.submitted, true);
        assert.ok(Number.isSafeInteger(step.opening_receipt.returned_ms));
        nativeControl(guard, { command: 'revoke_authority', permit: opening.permit });
        step.opening_permit_revoked = true;
        const menuRead = await observation(watch, ready, value => value.structure.complete && value.sequence > baseline.sequence &&
          value.structure_finished_unix_ms > step.opening_receipt.returned_ms && value.structure.menu_context,
        'same watch menu source after actual opening return');
        assert.equal(menuRead.stream_id, baseline.stream_id); assert.equal(menuRead.structure.menu_context.opener.anchor_id, anchor.anchor_id);
        assert.equal(menuRead.structure.menu_context.source_session_id, anchor.source_session_id);
        step.source_before = { stream_id: menuRead.stream_id, sequence: menuRead.sequence, observed_ms: menuRead.structure_finished_unix_ms,
          context: menuRead.structure.menu_context }; step.opened = await fixtureCommand(fixture, 'stats');
        assert.equal(step.opened.action_calls, 1); assert.equal(step.opened.selection_calls, 0); assert.equal(step.opened.menu_visibility_verified, true);
        const matches = menuCandidates(menuRead.structure, itemLabel);
        if (scenario === 'duplicate-label') {
          assert.ok(matches.length > 1, 'real Source records duplicate native menu labels');
          step.category = 'actual-source-test-binder-refusal'; step.binding_refusal = 'target_ambiguous';
          step.native_selection_submissions = 0; step.production_binding_reducer_verified = false;
        } else {
          assert.equal(matches.length, 1, 'unique real Source menu label'); assert.equal(matches[0].enabled, true);
          assert.equal(matches[0].in_view, true); assert.equal(matches[0].secure, false); assert.ok(matches[0].actions.includes('press'));
          const scope = { kind: 'item', context: canonicalContext(menuRead.structure.menu_context),
            opened_by: { attempt_id: attempt.id, intent_id: opening.intent_id, attempt_deadline_ms: ceiling } };
          let selection = packet(menuRead, matches[0], 'ax_menu_press', scope, attempt, ceiling, 'select', guard);
          if (scenario === 'source-session-restart') {
            await endChild(watch); watch = startWatch(ready, scenario + '-restarted-watch');
            const restarted = await observation(watch, ready, value => value.structure.complete && value.structure.menu_context,
              'actual restarted Source menu context', 3000).catch(error => { step.source_restart_error = String(error); return null; });
            if (restarted) {
              assert.notEqual(restarted.structure.menu_context.source_session_id, anchor.source_session_id);
              const candidates = menuCandidates(restarted.structure, itemLabel); assert.equal(candidates.length, 1);
              selection = packet(restarted, candidates[0], 'ax_menu_press', { ...scope, context: canonicalContext(restarted.structure.menu_context) },
                attempt, ceiling, 'restarted-select', guard);
              step.selection_receipt = await submit(guard, selection, scenario); rejected(step.selection_receipt);
            } else {
              step.category = 'actual-source-continuity-refusal'; step.native_selection_submissions = 0;
              step.binding_refusal = 'no_current_menu_evidence_after_watch_restart';
            }
          } else {
            if (scenario === 'stale-deadline') await pause(selection.duration_ms + 60);
            if (scenario === 'closed-context') step.fault = await fixtureCommand(fixture, 'close-menu');
            if (['anchor-replacement', 'foreign-geometry', 'disabled-item'].includes(scenario)) step.fault = await fixtureCommand(fixture, controls[scenario]);
            if (scenario === 'same-pid-other-window') step.fault = await fixtureCommand(fixture, 'occlude');
            if (scenario === 'changed-ceiling') {
              // Explicit hostile request mutation, never presented as Source truth.
              selection.original_proof.menu_scope.opened_by.attempt_deadline_ms += 1;
              selection.current_proof.menu_scope.opened_by.attempt_deadline_ms += 1;
              step.hostile_mutation = 'opened_by.attempt_deadline_ms + 1';
            }
            step.selection_request = selection; save(); step.selection_receipt = await submit(guard, selection, scenario);
            if (scenario === 'applied') {
              if (step.selection_receipt.outcome !== 'applied') {
                step.unexpected_selection_diagnostic = await unexpectedSelectionDiagnostics(guard, step.selection_receipt); save();
              }
              assert.equal(step.selection_receipt.outcome, 'applied'); assert.equal(step.selection_receipt.submitted, true);
              const resultRead = await observation(watch, ready, value => value.structure.complete && !value.structure.menu_context &&
                value.structure_finished_unix_ms > step.selection_receipt.returned_ms && value.structure.objects.some(object =>
                  object.role === 'AXMenuButton' && object.value?.type === 'text' && object.value.value === itemLabel), 'actual fresh AX selection result');
              step.result_source = { sequence: resultRead.sequence, observed_ms: resultRead.structure_finished_unix_ms, structure: resultRead.structure };
              const duplicate = await submit(guard, selection, 'same-intent duplicate'); rejected(duplicate); step.duplicate_receipt = duplicate;
              const next = structuredClone(selection); next.request_id = 'after-completion-' + randomUUID(); next.intent_id = next.request_id;
              next.permit = { id: next.request_id + '-permit', generation: 1 };
              const freshStart = Date.now(); next.started_ms = freshStart; next.latest_start_ms = freshStart;
              next.duration_ms = Math.min(1000, ceiling - freshStart); assert.ok(next.duration_ms > 100);
              next.valid_until_ms = freshStart + next.duration_ms; next.command_end_ms = next.valid_until_ms;
              step.new_permit_request = next;
              step.new_permit_receipt = await submit(guard, next, 'same-attempt second selection'); rejected(step.new_permit_receipt);
              assert.notEqual(step.new_permit_receipt.outcome, 'expired', 'consumed attempt rejection, not an expired test request');
            } else if (scenario === 'unknown-selection') {
              if (!['applied', 'unknown'].includes(step.selection_receipt.outcome)) {
                step.unexpected_selection_diagnostic = await unexpectedSelectionDiagnostics(guard, step.selection_receipt); save();
              }
              assert.equal(step.selection_receipt.outcome, 'unknown'); assert.equal(step.selection_receipt.submitted, true);
              await until(() => fixture.records.find(value => value.kind === 'selection_started'), 'actual delayed AXPress entry');
              await pause(250);
              const actual = await fixtureCommand(fixture, 'stats');
              assert.equal(actual.selection_ax_press_calls, 1); assert.equal(actual.delay_selection_entry_observed, true);
              step.actual_unknown_entry = actual;
              step.duplicate_receipt = await submit(guard, selection, 'unknown same-intent replay'); rejected(step.duplicate_receipt);
              const next = structuredClone(selection); next.request_id = 'after-unknown-' + randomUUID(); next.intent_id = next.request_id;
              next.permit = { id: next.request_id + '-permit', generation: 1 };
              const freshStart = Date.now(); next.started_ms = freshStart; next.latest_start_ms = freshStart;
              next.duration_ms = Math.min(1000, ceiling - freshStart); assert.ok(next.duration_ms > 100);
              next.valid_until_ms = freshStart + next.duration_ms; next.command_end_ms = next.valid_until_ms;
              step.new_permit_request = next;
              step.new_permit_receipt = await submit(guard, next, 'unknown same-attempt new permit'); rejected(step.new_permit_receipt);
              assert.notEqual(step.new_permit_receipt.outcome, 'expired');
              const reopen = structuredClone(opening); reopen.request_id = 'reopen-after-unknown-' + randomUUID(); reopen.intent_id = reopen.request_id;
              reopen.permit = { id: reopen.request_id + '-permit', generation: 1 };
              reopen.started_ms = freshStart; reopen.latest_start_ms = freshStart; reopen.duration_ms = next.duration_ms;
              reopen.valid_until_ms = next.valid_until_ms; reopen.command_end_ms = next.command_end_ms;
              step.reopen_request = reopen;
              step.reopen_receipt = await submit(guard, reopen, 'unknown same-attempt reopening'); rejected(step.reopen_receipt);
              assert.notEqual(step.reopen_receipt.outcome, 'expired');
            } else { rejected(step.selection_receipt); }
          }
        }
        await pause(250); step.after = await fixtureCommand(fixture, 'stats');
        assert.equal(step.after.action_calls, 1, 'no second native ShowMenu');
        if (scenario === 'applied') assert.equal(step.after.selection_calls, 1);
        else if (scenario === 'unknown-selection') { assert.equal(step.after.selection_ax_press_calls, 1); assert.ok(step.after.selection_calls <= 1); }
        else assert.equal(step.after.selection_calls, 0, 'boundary refusal emits no menu selection');
        for (const [path, hash] of Object.entries(evidence.build)) assert.equal(sha(path), hash, 'binary changed during run: ' + path);
        step.status = 'passed'; save();
      } finally {
        if (timer) { clearInterval(timer); timers.delete(timer); }
        if (guard?.proc.exitCode == null && guard?.proc.signalCode == null) { try { nativeControl(guard, { command: 'shutdown' }); } catch {} }
        if (watch) await endChild(watch).catch(() => {});
        if (fixture.proc.exitCode == null && fixture.proc.signalCode == null) {
          try { await fixtureCommand(fixture, 'close-menu'); fixture.proc.stdin.write('quit\n');
            await until(() => fixture.proc.exitCode != null || fixture.proc.signalCode != null, fixture.label + ' quit'); } catch {}
        }
        if (guard) await endChild(guard).catch(() => {}); await endChild(fixture).catch(() => {}); activeFixture = null;
      }
    }
    evidence.ok = true; evidence.tested_at = new Date().toISOString();
    evidence.preserved_hashes = preservedFileHashes(output);
    evidence.limits = ['Direct native helper boundary acceptance, not public Core/Engine or full MVP acceptance.',
      'Duplicate-label refusal is the test binder; production canonical BindingReducer requires separate acceptance.',
      'Source restart may refuse evidence before a native request; no stale record is relabeled as fresh.',
      'Unknown selection requires actual delayed AXPress entry and raw unknown receipt; unsupported AppKit override routing fails the run.',
      'Root-window stream pixels are not claimed to contain the auxiliary native menu.'];
    save(); console.log(JSON.stringify(runMetadata(true, artifact, output)));
  } catch (error) {
    evidence.error = String(error); evidence.failed_at = new Date().toISOString();
    try { if (activeFixture) evidence.failure_stats = await fixtureCommand(activeFixture, 'stats'); } catch (failure) { evidence.failure_stats_error = String(failure); }
    evidence.child_status = children.map(entry => ({ label: entry.label, pid: entry.proc.pid, exit_code: entry.proc.exitCode, signal: entry.proc.signalCode, parse_error: entry.parse_error, stderr: entry.stderr }));
    evidence.preserved_hashes = preservedFileHashes(output);
    save(); console.error(JSON.stringify(runMetadata(false, artifact, output))); throw error;
  } finally {
    for (const timer of timers) clearInterval(timer);
    for (const entry of children) {
      entry.proc.stdin.destroy(); if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGKILL');
    }
    await Promise.all(children.map(entry => entry.closed ? undefined : new Promise(resolve => entry.proc.once('close', resolve))));
  }
}

if (options['self-test']) selfTest();
else if (options['list-scenarios']) console.log(JSON.stringify({ scenarios, physical_input_run: false }));
else if (options.run) await run();
else console.log('Prepared only. Use --self-test or --run --fixture /absolute/path/to/MenuFixture.app/Contents/MacOS/MenuFixture.');

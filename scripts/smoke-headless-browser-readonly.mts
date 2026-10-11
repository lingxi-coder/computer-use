// Real, read-only adapter diagnostic; no task, permit or input is created here.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { parseArgs } from 'node:util';
import { launchOwnedBrowser } from '../apps/desktop/src/main/headless/ownedBrowser.ts';
import { HeadlessBrowserError } from '../apps/desktop/src/main/headless/cdpPipe.ts';
import { createBrowserPageSnapshot, HEADLESS_OBSERVATION_CLOCK_DOMAIN } from '../apps/desktop/src/main/headless/pageSource.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { 'read-only': { type: 'boolean' }, 'state-fixture': { type: 'boolean' }, chrome: { type: 'string' }, output: { type: 'string' } } });
if (!values['read-only'] || !isAbsolute(values.chrome ?? '') || !isAbsolute(values.output ?? '')) {
  throw new Error('Supply --read-only --chrome <absolute executable> --output <absolute new directory>.');
}
const chrome = values.chrome!;
const output = values.output!;
if (existsSync(output) && (!lstatSync(output).isDirectory() || lstatSync(output).isSymbolicLink() || readdirSync(output).length)) {
  throw new Error('Output must be a new or empty real directory.');
}
mkdirSync(output, { recursive: true, mode: 0o700 });
const fixtureSource = join(root, values['state-fixture'] ? 'fixtures/headless/checkbox-state-page.html' : 'fixtures/headless/readonly-page.html');
// The current private contract restricts the page basename. Keep that policy:
// the state matrix is an exact immutable copy in this run's owned fixture path.
const fixture = values['state-fixture'] ? join(output, 'fixtures/headless/readonly-page.html') : fixtureSource;
if (values['state-fixture']) {
  mkdirSync(join(output, 'fixtures/headless'), { recursive: true, mode: 0o700 });
  writeFileSync(fixture, readFileSync(fixtureSource), { flag: 'wx', mode: 0o600 });
}
const probe = join(root, 'target/native-macos/headless-display-probe');
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
const files = [chrome, probe, fixture, fixtureSource, fileURLToPath(import.meta.url),
  join(root, 'apps/desktop/src/main/headless/cdpPipe.ts'), join(root, 'apps/desktop/src/main/headless/ownedBrowser.ts'),
  join(root, 'apps/desktop/src/main/headless/pageSource.ts'), join(root, 'packages/desktop-api/src/browserValidate.ts'),
  join(root, 'packages/desktop-api/src/generated/browser.ts'), join(root, 'packages/desktop-api/src/generated/browser-schema.ts'),
  join(root, 'contracts/browser-v1/schema.json')];
const pins = Object.fromEntries(files.map(path => [path, hash(path)]));
const save = (name: string, value: unknown) => writeFileSync(join(output, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
const run = promisify(execFile);
type DisplayEvidence = { foreground_pid: number; foreground_visible_window_ids: number[]; foreground_visible_window_state: 'none' | 'present'; owned_browser_onscreen_window_count: number | null; focused_window_evidence: string };
async function display(pid?: number): Promise<DisplayEvidence> {
  const result = await run(probe, pid === undefined ? [] : [String(pid)], { timeout: 3000, maxBuffer: 8192 });
  const value: DisplayEvidence = JSON.parse(result.stdout);
  assert(Number.isSafeInteger(value.foreground_pid) && value.foreground_pid > 0);
  assert(Array.isArray(value.foreground_visible_window_ids) && value.foreground_visible_window_ids.length <= 512);
  assert(value.foreground_visible_window_ids.every(id => Number.isSafeInteger(id) && id > 0));
  assert.equal(new Set(value.foreground_visible_window_ids).size, value.foreground_visible_window_ids.length);
  assert.equal(value.foreground_visible_window_state, value.foreground_visible_window_ids.length ? 'present' : 'none');
  return value;
}
let session: Awaited<ReturnType<typeof launchOwnedBrowser>> | undefined;
let failure: string | null = null;
let closed = false;
let phase = 'before-display';
let ownedProfile: string | undefined;
const checks: Record<string, boolean> = {};
save('before.json', { created_at: new Date().toISOString(), pins, scope: 'owned Chrome headless readonly local fixture', input_enabled: false, provider_calls: 0, business_gate_eligible: false, mvp_complete: false });
try {
  const before = await display();
  save('display-before.json', before);
  phase = 'launch';
  session = await launchOwnedBrowser({ chromeExecutable: chrome, fixturePath: fixture, profileParent: output });
  const profiles = readdirSync(output).filter(name => name.startsWith('cu-headless-'));
  assert.equal(profiles.length, 1);
  ownedProfile = join(output, profiles[0]!);
  save('browser-owner.json', { pid: session.pid, executable: chrome, profile_parent: output, owned_profile: ownedProfile });
  phase = 'first-observation';
  const first = await session.observe();
  save('observation-1.json', first);
  const sourceOptions = { fixtureFileUrl: pathToFileURL(fixture).href, sessionEpoch: 1,
    sourceClockDomain: HEADLESS_OBSERVATION_CLOCK_DOMAIN, targetClockDomain: 'headless-diagnostic-logical-ms',
    mapMonotonicMs: (value: number) => value };
  const typedFirst = await createBrowserPageSnapshot(first, sourceOptions);
  save('page-source-1.json', typedFirst);
  assert.equal(typedFirst.observed_ms, Math.floor(first.provenance.readStartedMonotonicMs));
  assert.equal(typedFirst.read_finished_ms, Math.ceil(first.provenance.readFinishedMonotonicMs));
  assert.equal(first.document.url, pathToFileURL(fixture).href);
  assert(first.document.frameId && first.document.loaderId);
  assert(first.provenance.browserContextId && first.provenance.browserSessionId && first.provenance.sessionId);
  assert.equal(first.provenance.pageId, first.provenance.targetId);
  const nodes = first.rawAX.nodes;
  const checkbox = nodes.filter(node => node.role?.value === 'checkbox' && node.name?.value === 'Background option');
  assert.equal(checkbox.length, 1);
  assert(checkbox[0]?.backendDOMNodeId);
  assert(checkbox[0]?.properties?.some(property => property.name === 'checked' && property.value.value === 'false'));
  const disabled = nodes.filter(node => node.role?.value === 'button' && node.name?.value === 'Disabled action');
  assert.equal(disabled.length, 1);
  assert(disabled[0]?.properties?.some(property => property.name === 'disabled' && property.value.value === true));
  const typedCheckbox = typedFirst.nodes.filter(node => node.role === 'checkbox' && node.name === 'Background option');
  assert.equal(typedCheckbox.length, 1);
  assert.equal(typedCheckbox[0]?.checked, 'false');
  // Raw AX omits disabled=false. The closed read on this actual DOM node establishes it.
  assert.equal(typedCheckbox[0]?.disabled, 'false');
  assert(first.checkboxStates?.some(state => state.backendDOMNodeId === checkbox[0]?.backendDOMNodeId
    && state.connected && state.topDocument && state.nativeCheckbox && !state.disabled
    && !state.blockedByAriaOrInert && !state.checked && state.documentUrl === first.document.url));
  checks.actual_native_checkbox_state = true;
  if (values['state-fixture']) {
    for (const [name, expected] of [['Direct disabled', 'true'], ['Fieldset disabled', 'true'],
      ['Legend enabled', 'false'], ['Aria blocked', 'true'], ['Custom checkbox', 'unknown']] as const) {
      const match = typedFirst.nodes.filter(node => !node.ignored && node.role === 'checkbox' && node.name === name);
      assert.equal(match.length, 1, name);
      assert.equal(match[0]?.disabled, expected, name);
      assert.equal(match[0]?.checked, 'false', name);
    }
    // Inert content must not appear as an eligible nonignored checkbox.
    assert(!typedFirst.nodes.some(node => !node.ignored && node.role === 'checkbox' && node.name === 'Inert blocked'));
    checks.native_disabled_fieldset_legend_aria_inert_and_custom_matrix = true;
  }
  checks.actual_ax_targets = true;
  phase = 'screenshot';
  const shot = await session.capturePng(first.document);
  assert.deepEqual(shot.document, first.document);
  assert(shot.png.length > 1024 && shot.png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])));
  assert(shot.png.readUInt32BE(16) > 0 && shot.png.readUInt32BE(20) > 0);
  writeFileSync(join(output, 'page.png'), shot.png, { flag: 'wx', mode: 0o600 });
  save('screenshot.json', { document: shot.document, provenance: shot.provenance, bytes: shot.png.length,
    sha256: hash(join(output, 'page.png')), width: shot.png.readUInt32BE(16), height: shot.png.readUInt32BE(20), source: 'Page.captureScreenshot real Chromium rendering' });
  checks.actual_rendered_png_same_document = true;
  phase = 'second-observation';
  const second = await session.observe();
  save('observation-2.json', second);
  const typedSecond = await createBrowserPageSnapshot(second, sourceOptions);
  save('page-source-2.json', typedSecond);
  assert.equal(typedSecond.target_context_hash, typedFirst.target_context_hash);
  assert.deepEqual(typedSecond.context, typedFirst.context);
  assert.equal(typedSecond.nodes.length, first.rawAX.nodes.length);
  assert(typedSecond.source_sequence > typedFirst.source_sequence);
  assert(typedSecond.observed_ms > typedFirst.read_finished_ms);
  assert(typedSecond.snapshot_id !== typedFirst.snapshot_id);
  checks.typed_page_source_graph_context_and_read_spans = true;
  assert.deepEqual(second.document, first.document);
  assert(second.provenance.sequence > first.provenance.sequence);
  assert.equal(first.provenance.clockDomain, 'headless-performance-monotonic-ms');
  assert.equal(second.provenance.clockDomain, first.provenance.clockDomain);
  assert(first.provenance.readStartedMonotonicMs <= first.provenance.readFinishedMonotonicMs);
  assert(shot.provenance.readStartedMonotonicMs >= first.provenance.readFinishedMonotonicMs);
  assert(shot.provenance.readStartedMonotonicMs <= shot.provenance.readFinishedMonotonicMs);
  assert(second.provenance.readStartedMonotonicMs >= shot.provenance.readFinishedMonotonicMs);
  assert(second.provenance.readStartedMonotonicMs <= second.provenance.readFinishedMonotonicMs);
  assert.equal(second.provenance.browserSessionId, first.provenance.browserSessionId);
  assert.equal(second.provenance.browserContextId, first.provenance.browserContextId);
  assert.equal(second.provenance.pageId, first.provenance.pageId);
  checks.fresh_same_document_read = true;
  phase = 'during-display';
  const during = await display(session.pid);
  save('display-during.json', during);
  assert.equal(during.owned_browser_onscreen_window_count, 0);
  assert.equal(during.foreground_pid, before.foreground_pid);
  assert.deepEqual(during.foreground_visible_window_ids, before.foreground_visible_window_ids);
  checks.no_visible_browser_window = true;
  checks.foreground_pid_and_visible_windows_preserved = true;
  phase = 'cleanup';
  await session.close();
  closed = true;
  assert(!readdirSync(output).some(name => name.startsWith('cu-headless-')));
  const processRows = await run('ps', ['-axo', 'pid=,ppid=,args='], { timeout: 3000, maxBuffer: 16 * 1024 * 1024 });
  const leftoverCount = processRows.stdout.split('\n').filter(line => line.includes(ownedProfile!)).length;
  save('owned-process-cleanup.json', { unique_profile_process_count: leftoverCount, profile_removed: !existsSync(ownedProfile) });
  assert.equal(leftoverCount, 0);
  const after = await display();
  save('display-after.json', after);
  assert.equal(after.foreground_pid, before.foreground_pid);
  assert.deepEqual(after.foreground_visible_window_ids, before.foreground_visible_window_ids);
  checks.owned_child_profile_cleanup = true;
} catch (error) {
  failure = error instanceof Error ? error.message : 'unknown';
  if (error instanceof HeadlessBrowserError && error.diagnostic) {
    const { stage, commandMethod, nativeErrorCode } = error.diagnostic;
    save('failure-diagnostic.json', { stage, ...(commandMethod ? { commandMethod } : {}),
      ...(nativeErrorCode === undefined ? {} : { nativeErrorCode }) });
  }
} finally {
  if (session && !closed) {
    try { await session.close(); closed = true; }
    catch (error) { failure = `${failure ?? ''}; cleanup: ${error instanceof Error ? error.message : 'unknown'}`; }
  }
}
checks.pins_unchanged = Object.entries(pins).every(([path, expected]) => hash(path) === expected);
const result = { phase, ok: failure === null && closed && Object.values(checks).every(Boolean), error: failure, checks,
  owned_child_exit_confirmed: closed, provider_calls: 0, target_input_calls: 0, desktop_input_calls: 0,
  focused_window_acceptance: false, hidden_platform_windows_absent_claimed: false,
  normal_root_attempt_acceptance: false, headless_input_acceptance: false, business_gate_eligible: false, mvp_complete: false };
save('result.json', result);
console.log(JSON.stringify(result));
if (!result.ok) process.exitCode = 1;

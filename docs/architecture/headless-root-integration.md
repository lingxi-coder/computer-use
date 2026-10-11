# Headless page integration through the existing Root

This document tracks fixed-profile browser tasks through the existing Root,
Reaction, Execution and Verification owners. The bounded G fixed-checkbox flow
passed real Root and independent audit acceptance. H's ordinary Electron
product-entry flow for the single fixed local checkbox profile has now passed
live and independent audit. Three earlier H runs remain recorded as failures;
they are not overwritten by the accepted live-04 run. H does not establish full
MVP completion, arbitrary website control, or operation without WindowServer.

## Current implementation boundary

Main selects the closed `FixtureBackgroundOption` profile and launches one owned
headless Chrome with the fixed local fixture. A no-argument product control
replaces the previous session, cancels its Root, and starts observing; it does
not retain or resume the previous Root. Main chooses executable and fixture
paths, checks canonical regular assets, and fails closed when a packaged asset
is absent. Packaging configuration and signed distribution acceptance remain
open.

Main prepares a real document scope before starting Device. A private inherited
fd3 carries a one-use bootstrap, authenticated Hello/Ready, and exactly ordered
Submit/Reply frames. No token, listener address or page action is exposed to the
renderer, model, public Configure settings or ordinary Source. Device owns the
shared ExecutionCore and is the only owner that can authorize input. Main is
its leaf resource executor; it does not issue a second permit.

Main installs `BrowserReadScope` and publishes `BrowserPageSnapshot` over the
authenticated Supervisor-to-Host route, waiting for each correlated ACK. The
ordinary Source route rejects these messages. Environment owns page facts,
revision, timing floors and bindings independently of native-window evidence.
The profile permits only the registered checkbox Boolean goal with `CurrentUi`
confirmation and one `ui.ensure_value` skill. GoalDraft cannot choose the profile
or execution domain. The fixed local `browser_reference` planner supports only
`Enable browser fixture checkbox` and requires the current matching Device
capability. It is a deterministic fixture path, with no model fallback.

Browser candidates pass normal ReserveAttempt, durable unique decision mapping,
ActivateAttempt, permit and Execution revalidation. Desktop v2 uses the closed
native/browser Observation, Intent and SubmittedAction variants. The private
Device backend returns correlated pending completions; native capabilities
remain unavailable in this profile. The Main executor rechecks document,
native-checkbox state, backend identity, layout and exact hit target before one
`Input.synthesizeTapGesture`. It uses no Runtime script, DOM write, focus, scroll,
keyboard or operating-system pointer injection.

`Receipt.Completed` records input completion only. Verification requires a new
Environment-admitted snapshot whose actual read start is strictly later than
the input end, with the same scope/document/node and known expected state. It
persists `browser.checkbox_state@1` with `CurrentUi` and unknown causality. An
already satisfied task settles only from a read created after the attempt and
with a strictly newer Source sequence. That no-input result does not persist a
synthetic input fence or invent an intent, receipt or input span.

Stop, EOF, protocol failure or an ambiguous submitted action permanently retire
the private channel and scope. Synchronous transport shutdown is separate from
physical Chrome exit/profile cleanup. A reply grace cannot extend input
authority; final Input write must still pass the original cutoff and active
resource guard. Unknown actions are isolated and never replayed. A new profile
session cannot rebind the old Device scope.

H live-02 retains a mode-boundary failure: switching the same product session
from Observe to Execute permanently retired the page Source scope, so the next
same-target Root could not obtain a fresh read. The Rust mode-boundary fix now
keeps the active scope while invalidating the old binding and retaining the
source freshness floor; protected RootSource98 remains unchanged. A genuine
read that starts before the floor receives a typed negative ACK. The host may
perform only a bounded read-only retry after the clock, wall/performance and
Host barrier permit it; an input action is never retried.

The preceding shared-Core slice is frozen in the
[execution-core checkpoint](../../artifacts/headless-execution-core-9f2259e5-9ac4-4074-b75e-2f98a983b964/execution-core-checkpoint.json).
The current Root integration and actual input acceptance are tracked in
[the G artifact directory](../../artifacts/headless-root-runtime-8d91c90f-36b2-4be0-a294-28799dd2e00d/).
Only completed live results and independent audits establish actual behavior;
compile checks, fixture planners and fake-CDP tests do not substitute for them.

## G fixed-profile Root acceptance

[Live 08](../../artifacts/headless-root-runtime-8d91c90f-36b2-4be0-a294-28799dd2e00d/live-08/result.json)
and its [independent audit](../../artifacts/headless-root-runtime-8d91c90f-36b2-4be0-a294-28799dd2e00d/live-08/independent-audit.json)
passed with two ordinary tasks. The first produced one correlated gesture at
755–779 ms, followed by an admitted read at 802–805 ms (sequence 2 to 3). The
second settled from a later fresh read without an intent, receipt or input span.
Both durable effects are `Satisfied / CurrentUi`; the submitted action has
unknown causality and the zero-input effect has not-applicable causality.

The admission marker becomes delivered only on a matching Device receipt. A
settled, activated, refunded zero-input attempt correctly retains an undelivered
marker; it does not represent a pending input command. The consumed decision
and terminal attempt cannot be admitted again.

The foreground PID and ordinary visible-window samples remained 163 and an
explicitly known empty set. Chrome had zero on-screen windows. No AX focused
window was queried. Stop invalidated the scope; all four owned process IDs,
profile and Supervisor directory were independently confirmed gone. This is
acceptance on the current macOS desktop, not acceptance without WindowServer.
No provider call occurred. The local fixture planner is separate from live
model and full MVP quality acceptance.

## H normal Electron product-entry acceptance (accepted for the fixed profile)

The ordinary Electron product-entry flow passed live and independent audit in [H live-04](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-04/result.json), with its [runner record](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-04/runner.json) and [independent audit](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-04/independent-audit.json) all reporting success. The run used the real Main, preload, renderer profile picker, Start flow and goal confirmation ID. It changed from Execute to Observe, started a new goal, then executed again. Two ordinary Roots completed: one actual browser gesture and one same-target no-input completion.

Root 1 submitted one correlated `SetCheckbox(true)` gesture from 1308–1329 ms. A strictly newer Source read at 1342–1346 ms (sequence 2 to 3) confirmed the checkbox on, and `CurrentUi` verification recorded a satisfied effect with unknown causality. Root 2 activated, settled and refunded its one attempt with no intent, no new receipt and no input. Its independent read at 1661–1664 ms (Source sequence 5) showed the target still on. The no-input audit basis is attempt `created_ms` plus a strictly newer Source sequence; no persisted synthetic step fence is claimed. The live confirmation copy correctly described a fresh read of the current page; the earlier UiRevisit wording was fixed and covered by the [focused regression test](../../apps/desktop/test/viewModel.test.ts).

Three earlier failures remain preserved and are not counted as H acceptance:

- [Live-01](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-01/result.json) timed out waiting for the public product projection after the fixed-profile picker. No Root or browser input occurred. Its [independent cleanup audit](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-01/independent-failed-cleanup.json) found no remaining owned processes or directories.
- [Live-02](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-02/result.json) completed Root 1 with one gesture and fresh `CurrentUi` read, then timed out on Root 2 after Observe→Execute had retired the page Source scope. Its [independent cleanup audit](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-02/independent-failed-cleanup.json) found no remaining owned processes or directories.
- [Live-03](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-03/result.json) failed during CDP startup at `Target.setDiscoverTargets`, with zero reads, Roots or input. Its [independent cleanup audit](../../artifacts/mvp-product-entry-958e16c0-ff89-4653-938c-9ef721057c29/live-03/independent-failed-cleanup.json) found no remaining owned processes or directories.

The live-02 failure exposed the mode-boundary bug; live-04 exercises the fix. The active page Source scope now survives mode changes, the old binding is invalidated, and the freshness floor is retained. A typed actual read before the floor receives a negative ACK; only a bounded read-only retry may follow after the maximum clock, wall/performance and Host-barrier constraints permit it. The browser input is never retried. RootSource98 and SDK pins remained unchanged.

Main's [`browserDiagnostics`](../../apps/desktop/src/main/browserDiagnostics.ts) hook emits cloned, frozen, read-only owner, snapshot and execution records; it does not grant authority or feed decisions back into execution. `showInactive` is used only in the [test harness](../../apps/desktop/test/manual/headlessProductSmoke.cjs) to keep the real Console inactive, not in product startup. The display baseline was captured after Console readiness and before profile selection, not at app startup. Across four probe samples the foreground remained PID 163 with an empty visible-window set; Chrome PID 53501 had zero on-screen windows. No AX focused-window query was made, and no no-WindowServer claim is made.

The independent audit confirmed exit of Electron 53438, Chrome 53501, and Rust processes 53523, 53524 and 53525, with no owned directories remaining. H's final validation passed 1381 Rust tests (63 all-target suites plus 10 doc-test suites), 429 desktop tests, Clippy, type generation, typechecks, formatting, binaries and the Electron build. This accepts H only for the ordinary Electron entry and single fixed browser profile. It does not change any §26 or §28 gate or establish full MVP completion.

Wire interpretation follows the [CDP protocol schema](https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/master/json/browser_protocol.json):
Document URL is decoded from the strings table; AX frame ID and hit-test frontend
node ID are optional while backend/frame/document checks remain mandatory.
Chromium's [attribute builder](https://chromium.googlesource.com/chromium/src/+/3ca21435ac073fe124ed3665100eb771964c7776/third_party/blink/renderer/core/inspector/inspector_dom_snapshot_agent.cc#607)
uses its empty-string sentinel for boolean attribute values. Only attribute
values accept that sentinel. The non-element Document layout row may have no
CSS styles; every element row retains the full required style checks.
Historical failed runs and their cleanup audits are retained in the G artifacts.

## Observation and binding boundary

`StructureSnapshot` and `EnvSnapshot` retain their native app, window, focus,
and geometry requirements. Browser page evidence stays in its own typed
`Environment` state: it is not a fabricated `window_id`, native fingerprint,
coordinate space, or AX witness. The trusted session supplies the exact
approved context and logical clock domain; a snapshot cannot select its own
authorization. Browser session, context, page, CDP session, frame, document
loader, and session epoch are bound into the target context and its computed
hash. Snapshot sequence and read interval are tracked separately from that
identity.

The browser binding reducer derives only the supported checkbox binding from
the observed page facts and connects it to the existing live `BindingRef` and
semantic target. A usable binding must identify one checkbox with explicitly
observed enabled state and known checked state in the exact active document
and source sequence. Missing or conflicting state remains unusable. This is
read-only binding evidence and grants no input by itself. Only the fixed
profile's separate capability and execution checks permit its single checkbox
action; the corresponding `CurrentUi` effect is limited to that profile.

## Prior implementation plan (before Root integration)

This section preserves the requirements used before the current integration.
The host must select an approved page profile independently from model
proposals. At that checkpoint, `GoalSpec` rejected the `HeadlessBrowser` domain. A future
change may admit a narrowly supported checkbox goal only together with its
registered browser skill, binding, and capability. This requires a separate
design and implementation; the observation-only path at that checkpoint did not
open it.

Any future browser action must enter through ordinary Reaction and
`TaskRuntime::admit_attempt`, preserving durable attempt admission, decision
consumption, cancellation, and audit correlation. Deliberation cannot own
Chrome, submit CDP, select a domain, or change page authorization. Native
`ActionIntent` fields remain required. Desktop v2 now uses
`ExecutionObservation`, `ExecutionIntent`, and `SubmittedAction` with closed
`native` and `browser` variants. `BrowserExecutionObservation` and
`BrowserActionIntent` bind one enabled checkbox, its exact page scope, source
interval, dependencies, live binding reference, deadline, attempt, root, and
permit. The only browser action is `SetCheckbox`; it must request a change from
the observed checked state. Execution rejects cross-domain observations and
intents. The browser profile derives its initial epoch and surface from the
trusted scope; reconnect retires that scope and requires a new profile.

At that checkpoint, Root dispatch and the production Device resource port still
needed an executor that independently resolved the current node and document
before a closed page-local action. The fixed-profile integration now has that
bounded path; broader browser actions remain outside this contract. It preserves
Root/attempt correlation, permit, lease, Stop, and cancellation semantics. A
lost response becomes Unknown, isolates the action, and is never replayed.
Page-local actions do not emit desktop mouse or keyboard events.

## Wire, verification and acceptance gates

The shared `Message` union already carries the read-only browser scope,
invalidation, and page snapshot messages. They are validated and handled only
on the dedicated Supervisor route. Browser Source peers do not receive browser
control authority and cannot inject browser snapshots through the ordinary
Source route. Desktop v2 defines typed execution messages, without granting an
input capability. The v1 envelope schema and fixtures are retained as historical
artifacts; the running protocol rejects v1 envelopes and untagged v1 execution
payloads. Native private protocols and I0 keep their existing versions.
Production browser execution uses authenticated peer routing; all peers in a
packaged release must use the same closed contract. Supervisor
configuration uses its existing stdio exchange, not an added Hello handshake.

The fixed checkbox path has a bounded `CurrentUi` verifier: after input, an
independently admitted Source read must start later, use a newer sequence, and
match the exact session, document and node with known enabled and checked state.
The G live result and H live-04 first Root demonstrate that narrow path; the
effect is separate from the input receipt and causality remains unknown. This
does not establish a general business verifier for arbitrary browser actions,
targets or skills. A completed CDP command alone cannot complete a Goal.

The bounded G Root flow and H's ordinary-product entry flow have passed for
the fixed profile. H live-04 and its independent audit establish two completed
Roots across the Observe/Execute boundary; live-01/02/03 remain preserved as
failures. Broader acceptance still requires the §26/§28 model, task, physical
Stop, performance, resource and packaging gates; no single-page result
replaces them.

## Typed execution validation

The execution-core checkpoint is
[`headless-execution-core`](../../artifacts/headless-execution-core-9f2259e5-9ac4-4074-b75e-2f98a983b964/execution-core-checkpoint.json).
It records 1341 passing Rust tests across 63 suites, 375 desktop tests, 33 API
tests and 18 worker tests with one skip. Strict Clippy, format, type generation,
type checks, Rust binaries and the Electron build passed. Browser core tests
cover read-finish admission, exact-boundary acceptance, wrong scope/domain,
changed checkbox state, lease/Stop/cancellation, malformed replies, retained
in-flight recovery fencing, fixed timeout abandonment and no replay. These are
fake-backend lifecycle checks.

The earlier v2 Source/Host read-only regression passed in a real owned Chrome
session. Its two 15-node reads retained intervals 1381–1385ms and 1410–1414ms
and were independently validated by Rust. That source-ingress run had no page
input or Root/Attempt; later G and H results are tracked separately above. The
foreground baseline used by the product smoke is captured after Console is
ready, with PID 163 and an empty ordinary visible-window set. Chrome had zero
on-screen windows. No AX focused-window identity or no-WindowServer claim is
made.

## Current code ownership

- `crates/interaction-contracts/src/desktop/{browser,browser_execution,device,message}.rs`
  defines read-only browser evidence and tagged execution messages.
- `crates/execution-core/src/{core,backend}.rs` owns the shared execution
  lifecycle and typed browser backend seam; the fixed profile allows only its
  validated checkbox action, not general page input.
- `crates/interaction-engine/src/environment/{browser,state_core}.rs` owns admitted
  browser facts and read-only projection state.
- `apps/desktop/src/main/headless` owns the browser resource and read adapters;
  `apps/desktop/src/main/supervisor.ts` routes scope controls and snapshots to
  Host with correlated acknowledgements.
- `crates/desktop-host/src/shell.rs` handles browser messages on the dedicated
  Supervisor route; `crates/desktop-host/src/bridge.rs` rejects browser
  controls and snapshots arriving from ordinary Source peers.
- `crates/interaction-engine/src/{execution,verification}`
  integrate the fixed checkbox path and `CurrentUi` effect; general browser
  action and verification support remain outside the accepted profile.

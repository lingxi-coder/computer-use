# Background AX and headless control

Status (2026-10-07): one visible, owned Background AX diagnostic passed through
Main, Source, Device and the normal Root/Attempt lifecycle. The fixture explicitly
implements a cooperating background AX callback. Generic third-party controls,
hidden/minimized windows and arbitrary headless actions have not passed acceptance.
The separate [headless read-only foundation](headless-browser-adapter.md) now has
real page AX/PNG and no-visible-UI evidence. The [fixed checkbox Root path](headless-root-integration.md)
has passed real page input and independent fresh readback through normal Main,
Reaction, durable Attempt and Device. It does not establish arbitrary website
control or operation without WindowServer.
The preceding scoped-consent checkpoint remains immutable; MVP remains open.

## Product behavior

A task targets an application context independently of the OS foreground focus.
Background AX operations should address the selected application's actual
accessible element without moving the pointer, raising its window or making the
application frontmost. Supported actions must be established per actual target;
an AX callback can be unsupported, fail, time out, or cause application-defined
focus changes. An API call alone does not guarantee background behavior.

Three cases have different evidence and execution requirements:

| Case | Intended backend | Evidence |
| --- | --- | --- |
| Visible window behind another app | Native background AX | Real process/window/element identity and fresh AX readback |
| Hidden or minimized native window | Target-specific AX capability, if observable | Actual hidden/minimized state and an accessible stable root; no invented pixels |
| Browser or app with no OS window | Headless browser or app integration | Owned backend session/page/document identity and genuine backend observations |

A nonexistent NSWindow has no native AX window root to bind. It must not be
represented by a synthetic CG window ID. Headless browser pointer/key actions
belong to its page viewport and do not use the desktop's global pointer or focus.

## Foreground coupling identified before this change

- `environment/row.rs::resolve_row_target` selects its window from active_layer.
- `environment/state_core.rs::row_window_root` requires the requested active
  window and same-read SourceWindowContext. `usable` also requires active_layer.
- `native/macos/AXTableScopeSearch.swift` requires target frontmost and the fixed
  focused window for its partial census.
- `native/macos/WindowContext.swift::begin/project` validates system focused
  application/window. `AXVisiblePrivacy.swift` requires active-layer alignment
  for the existing partial pixel proof.
- `native/macos/Accessibility.swift::targetWindows` uses on-screen CG windows;
  zero AX roots are currently combined with inventory failure.
- `native/device/AtomicRowSubmit.swift` and ordinary input use requireActive
  hit testing. Owned editing additionally proves the actual focused receiver.
- `verification/mod.rs` couples ContextMatches to complete active-window evidence.
- `engine/stops.rs` pauses on human-input StopAck without target attribution.

The earlier implementation already had ordinary background AX enumeration and
restricted read-only stream capture. Those reads did not establish background
input authority. The dedicated BackgroundAx proof is now separate from the
WindowRootV1 activation proof; foreground Source/native internals retain their
focus and pixel checks.

## Ownership and contracts

Environment projects two independent facts: the operation's TargetContext
and the actual GlobalFocusObservation. Public snapshots no longer use active_layer.
Native Source derives known global focus from actual AX focus references and
matching CG metadata; unknown focus remains unavailable. A selected target does
not imply focus.
Background evidence must retain its own process/window or page/session identity,
generation, Source sequence, observation time and fault/refresh floors.

CapabilityManifest, observation purpose, binding proof, Device witness and
ActionIntent must identify the execution domain. The declared domains are foreground
native, background AX, headless browser, app integration and simulated. Physical
backend identity and an action's permitted domain are distinct. Contracts and
Device independently reject cross-domain proofs and unsupported actions.

All domains use the existing Root, frozen Goal, ReserveAttempt, durable decision
mapping, ActivateAttempt, permit, lease, cancellation, Stop, receipt and effect
verification lifecycle. They must not create a separate uncontrolled execution
loop. The foreground path retains activation. Background AX and headless paths
do not generate ActivateWindow as a prerequisite. Changing a domain invalidates
the previous binding and requires fresh admission.

Background AX currently supports one individually verified AXPress in a complete
visible standard-window tree, outside table, menu and scroll target chains.
AXSelected and AXValue require separate policies and acceptance. AXValue needs
settable/secure/composition checks and explicit action permission. It does not
inherit the owned editor's focus authorization. No global click, wheel, keyboard
or focus fallback follows an unsupported background action. Process-directed
events would be a separate future backend requiring its own acceptance.

Device must independently resolve the actual current element, validate PID and
kernel birth, authorized window/root, scope, element attributes and continuity,
then submit one bounded mutation. Slow queries stay outside the Stop state lock.
Unknown still isolates input and cannot replay. Verification requires a strictly
newer post-input observation in the same domain. Separate predicates establish
target-context match and actual foreground focus; a background task does not
assert WindowActive as its business result.

Human interference needs reliable target attribution. Global Stop, lease loss
and process/channel death remain unconditional. Target interaction or unknown
attribution pauses. Only proven unrelated activity in another app may continue
under a background-capable policy. This avoids both disrupting background tests
on every unrelated keystroke and silently ignoring target interference.

## Implementation order and evidence

1. Freeze domain capability and TargetContext contracts in
   `interaction-contracts/src/desktop/{device,binding,source,primitive}`; extend
   normal Engine planning, dispatch, binding and Verification without weakening
   the foreground validators. Source and Device capability are unavailable until
   their corresponding adapters exist.
2. Add a bounded background AX Source/Device path and a background-start option
   for the owned fixture. Begin with one visible owned checkbox AXPress outside table/scroll chains, with its actual callback behavior recorded.
   Prove the foreground PID/window is preserved, no activation/pointer/key call
   occurs, one native mutation has a matching receipt, and independent fresh
   Source evidence confirms the effect. This phase also exercises authorization,
   Stop, Unknown, process replacement and target interference.
3. Classify hidden, minimized, inaccessible and genuinely windowless targets
   explicitly. Only enable states/actions demonstrated by the target's AX
   implementation. Missing observations remain unavailable.
4. Implement a headless browser adapter through existing Source/Device ports.
   Use an owned browser context/page, real rendered screenshots or DOM evidence,
   page-local actions, and fresh readback. App integration is another adapter for
   programs that expose a real non-UI interface. Neither adapter impersonates
   macOS screen coordinates or native AX evidence.

Acceptance denominators stay distinct: simulated lifecycle tests, real headless
browser/app integration, real background AX, and foreground native desktop.
The `tc1` identity commitment uses compact JSON with a fixed field order. Its
window geometry is encoded as four 16-digit lowercase IEEE754 bit strings, with
either signed zero normalized to zero; the public frame stays numeric. This
avoids differences between Rust and Swift decimal float formatting. Seven
Rust-generated cases are checked by Native Device, including fractions, signed
zero and exponent-sized values.

Historical Source replay bytes remain unchanged. The separate
`scoped-window-synthetic.json` supplies assumed GlobalFocus only for unit tests;
it is excluded from live acceptance.

The trusted selected execution domain determines the frame requirement. Native
foreground execution still requires current pixels; BackgroundAx records the
absence of screenshots without advancing AX freshness floors. Domain changes
revoke old authority and require a new Source read. AX/observer/target faults,
Stop, unknown human interference and Unknown submissions retain their gates.

The owned diagnostic is `scripts/smoke-macos-background-ax.mjs --execute
--runtime <absolute Electron executable> --output <absolute new directory>`.
It uses real Main Cancel/Allow, separate Goal confirmation and normal task
execution. The Operator remains visible in front; the fixture stays behind it.
An independent probe checks the same foreground PID/window before and after,
while the evaluator checks the fixture action counter, native receipt, durable
attempt mapping and the original post-input Source snapshot. Its deterministic
reference planner is restricted to the registered Guard fixture and cannot be
combined with S2 model planning. It supplies no model or MVP business acceptance.

The [accepted run 21](../../artifacts/background-ax-5bcdf9e4-2d3d-4468-adf0-9a7f92675a2e/live-21/result.json)
and [independent saved audit](../../artifacts/background-ax-5bcdf9e4-2d3d-4468-adf0-9a7f92675a2e/background-live-independent-audit.json)
confirm one activated and settled attempt, one submitted AXPress, `applied` with
AXError 0, and one fixture action callback. The fixture changed to true with zero
pointer/key events. Foreground PID 12514 and window 25194 stayed identical before
and after; fixture PID 12563 was a different process. The original Source read
at 2148ms was strictly after the 1207ms input end. The frozen requirement was
Satisfied / CurrentUi / CausalityUnknown. The saved raw Source bytes have SHA-256
`d5ba97912f87fc8d29d048e9aaeba9716ee49011736c661f70dcaa32441a5cba`.
All binaries stayed pinned and the owned fixture exited.

This single result is specific to `BackgroundToggleButton`, an NSButton subclass
whose actual `accessibilityPerformPress` schedules its state change and action
on the main queue. The preserved vanilla NSButton runs 18 and 19 submitted one
real AXPress and changed the fixture, but returned AX timeout after about 33ms.
Their outcome stayed Unknown, the Root suspended, and no input replay occurred.
Those failures do not establish vanilla NSButton or arbitrary app acceptance.

The fixes required by the diagnostic preserve these boundaries:

- A Goal freeze requires fresh Source evidence. A refresh ACK captures the latest
  consumed Source sequence; execution remains blocked until a strictly newer
  valid read arrives. The ACK ends the refresh health exemption immediately.
- Native reads application-level AXHidden. The owned Guard fixture's AXZoomButton
  decoration closure may contain at most four AXGroup nodes over two group levels.
  Count-copy-count, actual CF parents, PID, typed roles, full graph continuity,
  deadlines and final checks remain mandatory; these groups cannot be candidates.
  A timely unsupported/no-value AXWindow shortcut may use the already-proven
  actual parent chain. Timeout, malformed data or mismatched references reject.
- Main uses the trusted authorized domain for presentation. Background execution
  keeps the console in place; restoring it from an old foreground indicator uses
  `showInactive`. Foreground execution keeps its original indicator behavior.
- After a known completed input, a ReadScoped binding can establish the effect
  using a new read's exact frozen target semantics and Source/process/window
  identity. This readback grants no input authority and never promotes causality.
  Unknown submissions cannot take this successful readback path or replay.
- Owned Native diagnostics contain a closed validation-stage enum and nine
  booleans, with no labels or free-form Source data; they grant no authority.

The [checkpoint](../../artifacts/background-ax-5bcdf9e4-2d3d-4468-adf0-9a7f92675a2e/background-ax-checkpoint.json)
retains all failed runs, standalone read-only AX probes, source/binary hashes and
check logs. Final validation passed: 1,297 Rust tests (60 result blocks), 291
desktop tests, 20 desktop-api tests, 18 decision-worker tests with one skip, 1,281
Native Device pure checks, 284 Native Source pure checks, and 105 mocked Stop
checks. Strict compilation, Clippy, formatting, type checks, generated contracts
and product/Electron builds passed. Mock Stop used 13 mocked calls and zero
physical input; it is not physical Stop latency acceptance.

Simulator/evaluator truth stays on the test side. These capabilities add useful
testing coverage while the original MVP section 26/28 gates remain open.

## Primary API references

- [Apple AXUIElementPerformAction](https://developer.apple.com/documentation/applicationservices/1462091-axuielementperformaction)
  addresses an accessible element and reports action support/timeout failures;
  target behavior still needs live verification.
- [Apple NSApplication.isHidden](https://developer.apple.com/documentation/appkit/nsapplication/ishidden)
  and [NSWindow.isMiniaturized](https://developer.apple.com/documentation/appkit/nswindow/isminiaturized)
  describe different window states.
- [Playwright browser modes](https://playwright.dev/docs/browsers) provide a real
  browser headless backend, separate from native desktop execution.

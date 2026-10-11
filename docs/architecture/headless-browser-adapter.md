# Headless browser adapter

Status (2026-10-07): real owned Chrome observations now pass through Main,
the normal Host control route, and Environment read-only bindings.
Headless input remains unavailable in the public protocol. The browser adapter
does not create a Root or an independent task execution loop.

## Real Host Source ingress

The [real run](../../artifacts/headless-source-ingress-1fc5eb38-fe77-4e3a-9667-468787be16c8/live-02/result.json)
and [independent audit](../../artifacts/headless-source-ingress-1fc5eb38-fe77-4e3a-9667-468787be16c8/live-02/independent-audit.json)
passed. Main installs a bounded `BrowserReadScope`, waits for the correlated
Host acknowledgement, then submits fresh `BrowserPageSnapshot` messages. Each
snapshot gets an acknowledgement only after Environment admission. Ordinary
Source peers cannot submit either browser control or browser snapshot messages.
No native window, geometry or focus fields are invented for this page.

Environment owns the active scope, original read interval, order floors,
browser revision and live `BindingRef`. Stop, recovery and a device session
identity change retire this evidence. Main also rejects outstanding requests
and closes its browser when Host exits. Publication waits for Host wall time to
cover the captured, rounded-up read finish; it never rewrites that interval.
An acknowledgement arriving after Stop cannot publish to the Main observer.

The two real reads retained 15 actual AX nodes, source sequences 2 and 3, and
read intervals 544–857ms and 882–886ms. Host installed the scope at 542ms and
projected browser revisions 2 and 3, each with a live read-only binding and
`checked=false`. Rust independently validated both original snapshots and
derived their bindings with `execution_authorized=false`. Stop cleared the
browser projection; the owned profile, Chrome and all three Rust children
were independently confirmed absent.

Foreground PID 1029 and visible-window set `[25239]` matched before, during
and after the run; Chrome PID 85450 had zero on-screen windows. Actual AX
focused-window identity was not queried. The normal ancillary Device and Source
processes used their simulated backends; their native-style observation is
recorded separately from the real browser evidence. No Root, Attempt, provider
call or page/desktop input was created. Normal desktop index/renderer setup,
page input authorization, browser skill, permit/lease, Device receipt and fresh
post-input business verification remain open.

The first startup timed out before a browser scope was installed. Its original
record is preserved; it did not capture the exact startup owner. A follow-up
found zero matching adapter profiles or project children, and the next run
captured ownership before launch. This is not a successful startup claim for
the failed run. Final checks passed: 1324 Rust, 375 desktop, 29 API and 18 worker
tests (one skip), strict Clippy, format, generated types, type checks, binaries
and Electron build. See the
[checkpoint](../../artifacts/headless-source-ingress-1fc5eb38-fe77-4e3a-9667-468787be16c8/source-ingress-checkpoint.json).

## Target and observation ownership

A browser page belongs to one newly launched browser process, private profile,
CDP browser context and target/session. Source observations retain those actual
IDs, a monotonically increasing local sequence, a monotonic read time and the
actual main-frame/document loader identity. A new document invalidates bindings;
the page target ID alone cannot establish document continuity.

The first implementation is deliberately read-only: `headless/ownedBrowser.ts`
launches an explicitly supplied Chrome executable with `--headless` and a private
inherited CDP pipe, reads the real accessibility tree and viewport metrics, and
captures rendered PNGs tied to the same document. It accepts one owned local
static HTML fixture. Scripts, downloads, external requests, new targets and
unapproved navigation are blocked. It exposes no arbitrary CDP, JavaScript or
input command. Pipe faults and timeouts retire the session without retrying.
Cleanup signals only the exact child it launched and removes its own temporary
profile only after child exit is confirmed.

The existing Electron runtime is another possible offscreen renderer, but its
hidden BrowserWindow does not by itself establish a working page adapter. The
current diagnostic uses the installed Chrome executable without installing a
browser, automation package or changing dependency pins. Chrome's unified
headless mode creates platform windows without displaying them; this proves
operation without visible UI, not absence of all platform-window objects.
See [Chrome's headless documentation](https://developer.chrome.com/docs/automation-and-testing/headless).

## Typed page Source and conservative read intervals

The follow-up [typed Source diagnostic](../../artifacts/headless-page-contract-c7c11a3e-e27f-4e53-b83a-e0bcfce2faeb/live-02/result.json)
and [independent audit](../../artifacts/headless-page-contract-c7c11a3e-e27f-4e53-b83a-e0bcfce2faeb/typed-source-independent-audit.json)
passed. `desktop/browser.rs` owns a native-free page snapshot and read-only
checkbox Binding/Witness contract; `contracts/browser-v1/schema.json` generates
TypeScript DTOs. A separate validator checks byte limits, safe integers, all
actual parent/ordered-child references, one root, connectivity, cycles and depth.
The Main mapper preserves ignored nodes and absent properties. Fixed-order
compact context JSON produces the same `tc1` SHA-256 in Rust and Web Crypto;
three Rust goldens cover ASCII, Unicode and escaped URLs.

OwnedBrowser now captures actual `performance.now()` at the start and finish of
each observation. A trusted caller maps both into its logical clock domain,
flooring start and ceiling finish. `observed_ms` is the conservative read-start
bound; a late return cannot make a pre-input read appear fresh. Read identity and
target-context hash are independent from these times and Source sequence.

The real run preserved all 15 AX nodes. Rust independently validated both emitted
page snapshots and their context hash
`tc1:7515a92392852085b98b09bd0f0fb517e201a5c7a1708e553a31991fb7586ccb`.
Its first read interval was 1428–1435ms; the second had a newer sequence and began
strictly after that interval. Raw AX did not report `disabled` for the checkbox:
the typed value stayed Unknown, and Rust refused to derive a binding. This does
not establish input authority or a normal Root/Attempt. Actual effective enabled
state still needs an independent bounded page observation before an input path
can be admitted.

The first startup timed out on `Target.setDiscoverTargets`; no inputs were sent,
the original failure remains, and independent cleanup passed. A new separate
session passed without extending the deadline. Focused checks passed: 218 Rust
contract tests, 33 related Node tests, full 318 desktop / 26 contract / 18
worker tests (one skip), type checks, contract Clippy, formatting, generation and
Electron build. No full Rust-workspace test rerun is claimed for this additive
private contract step. Existing desktop/computer schemas, SDK and dependency
pins remain unchanged. The
[checkpoint](../../artifacts/headless-page-contract-c7c11a3e-e27f-4e53-b83a-e0bcfce2faeb/page-contract-checkpoint.json)
freezes source, schemas, checks and live evidence.

## Required integration before input becomes available

The file-grounded implementation sequence is in
[Headless Root integration](headless-root-integration.md). Native observation
fields stay required; Environment owns browser facts and bindings, and browser
execution must share the existing durable admission, lease and Stop lifecycle.

Current `TargetContext::BrowserPage` is only an identity placeholder. Its hash
validation checks format and Source sequence/time, and does not recompute a
commitment over page identity. `StructureSnapshot::UiObject` also requires a
native-style `window_id`. A browser page must not be inserted using a fabricated
CG window, NSWindow or BackgroundAx witness.

The next integration must provide these dependency-complete pieces:

| Owner | Required change |
| --- | --- |
| Contracts | Page-specific bounded observation/tree and computed identity commitment; browser binding scope and witness; a closed page-local action; actual browser backend manifest and strict cross-domain validation |
| Main / Supervisor | Trusted page profile and explicit session/context/page/document authorization, independent of native-window grants; cancellation, navigation, replacement and exit revoke it |
| Source peer | Real page observation and health on existing authenticated IPC; refresh ACK followed by a newer admitted read; bounded media on the existing image path |
| Environment | Page-specific freshness and binding reducer; no global focus or desktop window prerequisite |
| Reaction / TaskRuntime | Frozen browser target and normal skill/Root lifecycle; model proposals cannot choose or elevate the authorized execution domain |
| Execution / Device | Existing ReserveAttempt, durable mapping, ActivateAttempt, permit/lease/Stop checks; independently re-resolve the actual current page node and document before one local action |
| Verification | Strictly newer post-input page observation; input completion is distinct from semantic effect; Unknown isolates input and never replays |

The first input slice should be one atomic action on a uniquely identified owned
checkbox outside scrolling, embedded frames or editable content. A page-local
click or activation needs its own typed witness; it must not be expressed as
macOS AXPress or a global desktop click. Device and Source failure domains stay
separate. Deliberation proposes through the existing runtime rather than owning
the browser or directly sending input.

## Evidence and remaining acceptance

`scripts/smoke-headless-browser-readonly.mts` checks actual local-page AX values,
document continuity and rendered PNG, then separately samples NSWorkspace and
CGWindowList for the foreground PID/visible-window set and absence of an
on-screen window owned by the browser. This read-only probe does not query the
actual focused AX window; it does not claim unchanged focused-window identity.
It records no inputs, permits or Root/Attempt, and is not headless input or MVP
business acceptance. Pure pipe tests are separate from this real browser probe.

The [final real run 05](../../artifacts/headless-browser-readonly-c53cd9f9-1869-4ebf-a4e3-04a15f9ba4da/live-05/result.json)
and [independent saved audit](../../artifacts/headless-browser-readonly-c53cd9f9-1869-4ebf-a4e3-04a15f9ba4da/headless-independent-audit.json)
confirm 15 actual AX nodes, one unchecked checkbox and one disabled button,
Source observation sequences 1 and 3 with a same-document screenshot at sequence
2. The actual rendered PNG is 756×469, 26,053 bytes, SHA-256
`a0001bf32d18fa4b7fadf1d2f8d6d9b4775fc5d901e343eaaccabe441abace07`;
it was visually inspected. Foreground PID 47953 and visible window set `[24908]`
stayed unchanged before, during and after. Owned browser PID 31749 had zero
on-screen windows. Child exit, profile deletion and absence of processes using
that unique profile were confirmed. No target or desktop input was issued.

Runs 01–03 rejected during target discovery; run 03 specifically recorded an
unexpected non-page target. The fixed discovery and roster requests now use a
page-only TargetFilter, and closing discovery omits the filter as specified by
[official Target.pdl](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Target.pdl).
Owner page/context/URL checks were retained. All failed runs are preserved and
independent cleanup checks passed. Run 04 also passed; run 05 used the final
TypeScript control-flow fix. Error diagnostics expose only closed stage/method
names and a bounded native code, with no raw error message/data or target labels.

Final checks passed: 16 focused headless tests, all 307 desktop tests, 20
contract tests, 18 decision-worker tests with one skip, strict workspace and
smoke-script type checks, standalone Swift 6/concurrency/warnings compilation
and Electron build. The first full Node command omitted the existing offline
Git mirrors and failed on an unpublished SDK revision; its owned descendants
were stopped, and the complete offline rerun passed with identical dependency
pins. Earlier type and test-harness failures are also preserved. Rust and public
contract code matched 242 frozen source pins from the Background AX checkpoint;
the prior 1,297 Rust tests are inherited, not rerun in this headless step.

The [headless checkpoint](../../artifacts/headless-browser-readonly-c53cd9f9-1869-4ebf-a4e3-04a15f9ba4da/headless-readonly-checkpoint.json)
records source/binary/artifact hashes and these acceptance limits.

The backend must remain unavailable until contracts, ownership and normal
execution are connected and accepted. Subsequent tests need wrong context/page,
navigation and document replacement, stale sequence, Stop/lease loss, pipe death,
lost acknowledgements and Unknown-without-replay before a real authorized input
and fresh readback can count as a headless execution sample. The original MVP
sections 26/28 remain open.

## Real effective checkbox state

The [base page](../../artifacts/headless-enabled-state-78265db1-b2a7-47bc-a460-e7d04c3e15bb/live-05/result.json),
[state matrix](../../artifacts/headless-enabled-state-78265db1-b2a7-47bc-a460-e7d04c3e15bb/live-07/result.json)
and [independent Rust/Source audit](../../artifacts/headless-enabled-state-78265db1-b2a7-47bc-a460-e7d04c3e15bb/enabled-state-independent-audit.json)
passed. Actual bounded DOM reads now supplement AX's omitted disabled=false.

A batch reads the complete top light-DOM graph, uses only fixed native checkbox
selectors, then reads the graph again. Positive membership establishes the
native input before complete `:disabled`, `:checked` and `:indeterminate` sets
provide state. Chrome supplies fieldset inheritance and the first-legend
exception, following the [HTML disabled-control rules](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#enabling-and-disabling-form-controls:-the-disabled-attribute).
ARIA/inert self or ancestors conservatively block input; mixed state stays
Unknown and custom ARIA controls stay AX-only. Conflicting evidence rejects.

Real `DOM.getDocument` calls reassign frontend IDs. They serve only queries in
one read; revalidation compares actual backend IDs and ordered ancestry. All
reads remain inside the frame/loader/URL and original timing bracket. No Runtime
evaluation, arbitrary selector or input API remains. Scripts stay disabled;
the child receives only bounded PATH/TMPDIR/locale environment values.

Limits are 2,048 light-DOM nodes, depth 24 and 32 selector results. Child documents
and author shadow roots fail closed, including slotted/inert ancestry. Neither
truncation nor missing membership establishes enabled state. User-agent internal
shadow nodes are excluded. This does not establish general dynamic-site input.

The base read preserved 15 AX nodes (1015–1030ms); the matrix preserved 33
(891–902ms). Actual direct and fieldset disabled controls, enabled first-legend
control, ARIA block, inert exclusion and custom Unknown matched expectations.
Rust validated both snapshots per run and derived the read-only Background
option binding with `execution_authorized=false`. Foreground PID 1029/window
set `[25239]` stayed unchanged before/during/after, with zero on-screen browser
windows and independent process/profile cleanup. Actual AX focused-window
identity was not queried. The rendered matrix PNG was visually inspected.

The matrix is an exact pinned copy under the current restricted
`fixtures/headless/readonly-page.html` basename. The contract restriction was not
broadened. Failures remain: startup timeout (01), fixed Runtime query rejection
(02), frontend-ID comparisons (03/04), matrix filename rejection (06). Runtime
reading was replaced completely with DOM-native queries. All failed sessions
have independent cleanup evidence; no input was issued or replayed.

Final checks passed: 70 focused cases, full 355 desktop/26 contract/18 worker
tests (one skip), workspace/smoke type checks, generated checks and Electron
build. Rust/public contracts and dependency pins match the previous checkpoint;
no fresh Rust-workspace run is claimed. The
[checkpoint](../../artifacts/headless-enabled-state-78265db1-b2a7-47bc-a460-e7d04c3e15bb/enabled-state-checkpoint.json)
freezes evidence. Production Source ingress, Environment binding, page approval,
typed intent/receipt, lease/Stop, normal Root and post-input effect remain
required. Headless input is still Unavailable; the original MVP is incomplete.

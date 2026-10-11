# Fresh capture for the local computer loop

The same-watch control, typed provenance and local integration are implemented.
The native Source timing and window-identity acceptance gates remain open.
Registered FORM-02 and actual Source/Main image-worker diagnostics establish
the bounded local paths described below; they do not establish every native gate.

## CMSampleBuffer timing diagnostic (2026-10-05)

A separate read-only diagnostic used public
`SCScreenshotManager.captureSampleBuffer` on the owned FORM-02 window, while
retaining the same `SCWindow`, filter, configuration and active `SCStream`.
[Raw result](../../artifacts/m4-capture-sample-timing-69cbfc02-6f07-4f11-8e4a-68eea8f0764b/sample-timing-02-result.json)
and [timing evidence](../../artifacts/m4-capture-sample-timing-69cbfc02-6f07-4f11-8e4a-68eea8f0764b/timing-evidence.json)
retain three screenshot buffers and two valid stream buffers. All visible-pixel
SHA-256 hashes matched, excluding row padding. Screenshot PTS values were
numerically within each 77.43–78.81 ms API interval, and 2.20–3.00 seconds after
the last recorded stream PTS. The screenshot buffers had no frame attachments;
they did not supply SCK display-time or status metadata.

This is empirical timing evidence for one static owned window. The local Apple
SDK documents the host clock as mach-absolute time and stream display time as
the WindowServer display event. The screenshot API documentation and
[WWDC23 explanation](https://developer.apple.com/videos/play/wwdc2023/10136/)
do not establish a screenshot PTS clock or actual acquisition-time guarantee.
Matching numeric ranges does not supply that contract. The production
`captureImage` route and its provenance are unchanged; actual acquisition
semantics and WindowServer ABA remain unverified.

The first diagnostic aborted during filter creation with `CGS_REQUIRE_INIT`.
Its [failed result](../../artifacts/m4-capture-sample-timing-69cbfc02-6f07-4f11-8e4a-68eea8f0764b/sample-timing-01-result.json)
and original diagnostic source are retained. The successful diagnostic first
called public `CGWindowListCopyWindowInfo` for the exact registered owned
window. Both owned processes exited. These probes started no Root or Attempt,
made no provider call, and posted no physical input.

The Responses computer loop needs an actual screenshot after the model's
screenshot request, or after the last completed local action. A healthy idle
stream callback does not prove that a new image was captured after that barrier.
`SourceCommand::RefreshNow` currently invalidates Source authority and restarts
the helper and stream. It cannot serve as a same-watch capture operation.

The previous watch helper required EOF after startup. The ordinary watch now
reads one bounded startup line and keeps stdin for bounded CaptureNow and
CancelCapture records. The registered owned-document bootstrap retains its
separate EOF-framed read path and does not inherit this capture capability.
The existing `captureWindow` helper uses `SCScreenshotManager`, but independently
resolves a window; its result has capture start/end timestamps rather than SCK
display timestamps. Reusing a numeric window ID alone does not prove that it is
the window instance selected by the active watch.

Add a live, private `CaptureNow` control request carrying a request ID, a strict
capture-after barrier and the original deadline. Keep one pending request per
watch. An acknowledgement means the request was accepted, not that the capture
has completed. Stop, watch replacement and helper exit cancel the request and
make its late result ineligible for publication.

The native capture must reuse the active watch's selected `SCWindow`, window
information, AX read scope and writer. Before and after capture, revalidate the
selected process, bundle, window instance, geometry, permissions and privacy
scope. Do not restart the watch or allocate a replacement authority to satisfy
the request.

Publish a separate single-capture provenance variant. It must identify the
capture request/result, selected window and PID, watch/session/surface, geometry,
shared clock, capture start/end interval, dimensions and checksum. Pair the
result with a current complete structure read from that watch. Preserve the
original SCK stream branch and its real display timestamps; do not manufacture
display ticks or label an idle event as a newly captured complete stream frame.

For the single-capture variant, use a conservative capture-start lower bound for
freshness. Require start strictly after the requested barrier, end at or after
start, and completion before the original deadline. Every continuation needs a
new capture/frame identity, a higher Source sequence and the exact metadata and
PNG pair. Clock conversion uncertainty must be represented rather than hidden
by a fabricated timestamp.

The change spans the Source/frame contracts, native Models/SourceMain/capture
implementation, Rust watch/shell/image tracking, Environment frame and frozen
observation validation, the private image codec, Main frame gate and the adapter
continuation validator. All branches must discriminate stream and single-capture
provenance explicitly. A partial implementation must remain non-executable.

The code map is `desktop/source.rs` SourceCommand, Host and Source shell command
dispatch, `source-macos/src/native/watch.rs`, and native `SourceMain.swift`,
`StreamCapture.swift`, `Capture.swift` and `Models.swift`. Keep startup/control
records bounded and separate from Source-refresh acknowledgement handling.
Retain the selected SCWindow/filter and AX window scope in the watch; the control
reader passes only Sendable request values to the existing serialized writer.

Environment currently combines stream heartbeat with content. Single capture
must update content while preserving the real SCK heartbeat, rather than
manufacturing a callback. Source's image server is latest-only without an ACK;
ordinary stream frames can overwrite a capture PNG before Main reads it. Add
request-scoped retention or reading for a completed capture. Exact metadata/PNG
rejection alone does not guarantee that the requested capture can be retrieved.

Native acceptance must also establish whether SCWindow instance continuity can
be proved across validation and whether the API call interval conservatively
bounds actual image acquisition. Numeric window ID, geometry and return time do
not by themselves establish these properties.

Acceptance needs a static window that produces no new stream pixels: a request
must obtain a genuinely newer same-watch image. Cover concurrent requests,
expired barriers, Stop during capture, geometry and window-instance changes,
helper death, late replies, and a subsequent Root that cannot consume a previous
Root's result. Deterministic protocol tests establish correlation and rejection;
actual native runs establish capture provenance and timing.

Computer action progress remains separate from task success. A completed local
action requires a known successful ordinary Attempt and input receipt; Unknown,
partial or denied input cannot advance the prefix. The next action still needs
fresh observation and ordinary admission. Only Verification of the frozen
RootGoal predicates establishes business success, and that success cancels any
remaining proposed actions.


## Local implementation and validation (2026-10-05)

`FrameMeta.provenance` is a closed `stream` / `single_capture` union. Its capture
branch carries the original request, watch, session/surface, window geometry,
mach-absolute operation interval, conservative mapped bounds and original
deadline. The private image request requires the exact capture request ID and
deadline. Schema, generated TypeScript and shared PNG packets use this contract.

Host retains acknowledgement correlation until cancellation or deadline;
acceptance neither supplies pixels nor acknowledges a Source refresh. Source
keeps one pending capture, rejects or cancels foreign/late results, and publishes
one compound PNG/AX result. Ordinary stream frames and a completed capture have
separate bounded image slots. Ending capture correlation keeps already published
immutable bytes available for the image pipe; source loss clears both slots.

Environment admits new capture content against the existing healthy watch and
keeps SCK callback liveness separate. Model thinking can retain its immutable
original observation while the watch stays healthy. Screenshot egress and input
admission still require fresh evidence. An expired proposal stays inert until a
new exact pair passes the existing compiler and durable Attempt checks.

The Swift watch retains its original SCWindow/filter and AXSession. A dedicated
nonblocking control reader hands only bounded Sendable values to the serialized
watch loop. Capture updates the loop's AX-read high-water mark. Cancellation or
stream loss after sequence reservation produces a failure at that same sequence
rather than silently skipping a record. The producer does not manufacture SCK
display times or declare a one-shot result to be heartbeat evidence.

The local checkpoint records 1066 workspace Rust tests, 35 scoped TypeScript
image/contract tests, 208 native offline checks, strict Clippy/format, generated
types, typechecking and Electron build. The readonly native attempt
`native-static-01.json` did not obtain initial COMPLETE pixels: startup AX
revalidation encountered an unresolved modal/transient active layer and the
helper withdrew screenshot health. It never reached CaptureNow acceptance, and
both owned processes exited. This failure is preserved, not counted as a pass.

WindowServer ABA/incarnation continuity and acquisition timing beyond the
producer-declared API-call bounds remain unverified. Concurrency/Stop/replacement
and hardware matrices remain required. The original startup failure above is
superseded for the registered FORM-02 producer scope by the follow-up below.

## Native producer follow-up (2026-10-05)

The registered FORM-02 static-window probe reached COMPLETE AX and SCK pixels.
Its first CaptureNow exposed a Swift runtime actor check: a Dispatch callback
created inside the MainActor reader initializer inherited MainActor isolation
despite running on the separate control queue. Explicit Sendable event and
cancellation callbacks fix that boundary. A real anonymous-pipe regression
initializes the reader from MainActor and exercises capture, cancellation, EOF
and descriptor flag restoration.

The next correlated result exposed the Retina geometry error. This fixture has
a 1160×768-point window, 1160×768 output pixels, SCK `scaleFactor = 2`,
`contentScale = 0.5`, and a 580×384 content rectangle. These raw fields remain
unchanged. Full-window validation checks both content-rectangle-to-buffer and
window-to-buffer transforms, allowing at most one pixel of rounding and no
padding. Pixel-to-screen conversion uses the product of display density and
content scaling. Rust, TypeScript and the native producer reject zero-area,
nonuniform, unrelated-scale and overflowing transforms.

The final native binary passed
[FORM-02 probe 05](../../artifacts/m4-native-same-watch-f3e77f15-0b79-4b8b-a96f-4975a39df6e3/native-form02-05-result.json):
a healthy idle stream was followed by a new capture PNG identity and complete
paired AX on the original watch, within the requested API-operation bounds and
original deadline. A subsequent idle callback retained its real, older SCK
display timestamp. Both owned processes exited. The probe is read-only and
uses only fixture launch registration for its private observation scope; it
does not start a Root or Attempt or use the business oracle for target selection.

Local checks now record 1071 workspace Rust tests (plus the final Source 77-test rerun), 36 scoped TypeScript tests
and 211 native checks, including Retina coordinate conversion for both stream
and single-capture branches. The full MVP acceptance remains open; the additional Source delivery evidence
is described below.

The subsequent
[product Source probe 04](../../artifacts/m4-native-same-watch-f3e77f15-0b79-4b8b-a96f-4975a39df6e3/product-source-04-result.json)
passed through real `cu-source`, the authenticated Host control socket and
Main's actual image worker. It received exact matching capture metadata and
checksum-validated PNG bytes, paired complete AX, then a later real idle event.
Source exited normally, the fixture exited, and the owned socket directory was
removed. Its diagnostic ComputerFence is synthetic and validates metadata only;
no Root or Attempt was started and no provider or physical input was invoked.
It does not prove the complete Electron/Host/Engine authorization flow.

Two Source defects were fixed on this path. The configuration already allowed
registered MVP fixtures, but duplicated window/focus/menu parser policy lists
omitted them. Those checks now share the configuration's same fixed three-app
list and retain all PID/window/session/kernel/context guards. The paired AX
read finishes before PNG publication. Its raw window/focus/menu proof stamps
must be checked against the nested observation header, whose watch and sequence
remain exactly correlated with the outer capture result. Capture additionally
rejects an AX read that started before the image operation finished. Tests
cover distinct read/publication timestamps, an old AX read and a coherently shifted nested clock. The nested AX header remains anchored to the outer capture clock within the same two-millisecond uncertainty.

Actual exposure timing beyond the API-call bounds, WindowServer incarnation
continuity, full cancellation/replacement and §26/§28 acceptance remain open.

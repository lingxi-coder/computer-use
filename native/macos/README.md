# Native macOS observation helper

This Swift 6 helper belongs to the source process. It never injects input. Build
it with `scripts/build-macos-source.sh`; the executable is
`target/debug/cu-macos-source`. Permissions and diagnostic observe require macOS
14 or newer; persistent watch requires macOS 14.2 to explicitly exclude child
windows from the target-window filter.

The caller writes one JSON request line (at most 8192 bytes) then closes stdin. The
helper writes bounded JSON lines (at most 768 KiB each) to stdout. `watch` is
persistent NDJSON; the other commands return one line. Diagnostics use stderr.

```json
{"protocol_version":1,"command":"permissions"}
{"protocol_version":1,"command":"self_test"}
{"protocol_version":1,"command":"watch","bundle_id":"com.example.fixture","output_dir":"/absolute/private/assets","timeout_ms":1500,"interval_ms":1000,"max_nodes":512,"max_depth":12,"max_ocr_blocks":128}
{"protocol_version":1,"command":"observe","bundle_id":"com.example.fixture","output_dir":"/absolute/private/assets","timeout_ms":1500,"max_nodes":512,"max_depth":12,"max_ocr_blocks":128}
```

`pid` optionally selects one running instance of the bundle. An absent, exited,
or ambiguous target produces an incomplete observation. Permissions use
`AXIsProcessTrusted` and `CGPreflightScreenCaptureAccess`; no permission request
or System Settings dialog is triggered.

AX references stay inside a dedicated source worker. A 40 ms process-wide AX
messaging timeout also covers descendants; traversal is limited to 512 objects,
12 levels and 750 ms (or half a shorter request budget). Children are fetched by
bounded range rather than an unbounded AXChildren read. Timeouts, invalid
geometry, ambiguous window matching, cycles and truncation produce
`complete=false`. During watch, a dedicated CFRunLoop owns AXObserver and a
bounded CFEqual instance registry. Context changes invalidate generations. If
required notifications are unsupported, direct reads can still be complete but
use new per-read IDs and report observer coverage unavailable. Unresolved
transient/modal layers have no active input layer.

Secure text values, titles and descriptions are never requested. Editable text
is private by default. A trusted `text_policy` request with
`{"kind":"registered_editor","window_id":"window-N"}` and a fixed PID can
expose normal TextField/TextArea values only in that CG-matched window of
GuardFixture or TextEdit. Secure/unknown subroles and other windows remain
private. Labels retain the 512-byte budget; registered values preserve exact
Unicode scalars within 2048 UTF-8 bytes. Overflow explicitly makes the read
incomplete. Capture/OCR remain withheld for incomplete trees or any sensitive
field. Direct AX health and notification coverage are reported separately by
Rust; absent lifetime coverage still requires new per-read IDs and bindings.
See the [registered text workflow](../../docs/architecture/m3-native-value-workflow.md).

`watch` uses SCStream for exactly the selected process and window. Its callback
copies bounded BGRA pixels and real SDK metadata into a bounded mailbox;
encoding, AX validation, OCR and stdout never run in that callback. `observe`
remains a diagnostic single-shot SCScreenshotManager path. Before saving pixels,
the source checks original capture time and selected window identity/geometry.
Diagnostic observe requires the target to remain frontmost. Watch can observe a
selected background window while keeping `active_layer` honestly absent; the
host then closes execution eligibility without misreporting a healthy stream.
Without an active layer, initial selection requires exactly one standard,
nonmodal window; ambiguous selection fails closed. A second complete AX read must show the same observed content/geometry and
no sensitive fields before the image can be saved or OCR can run. This rejects
observed changes; it does not guarantee discovery of content that appeared and
disappeared between both reads. Pixels are bounded to
2048×1536, PNG assets to 12 MiB, OCR to 128 blocks and 512 UTF-8 bytes per label.
Assets use fresh UUID basenames and exclusive, no-follow creation in a caller
owned directory with mode 0700; each asset has mode 0600. No existing file is
overwritten. Vision's lower-left normalized boxes are explicitly mapped to
global upper-left display points, including negative monitor origins.

SCStream Complete records contain original mach display ticks, their uptime
conversion, original callback clocks, dirty/content rectangles, scales and
geometry revision. Idle refers to the last accepted Complete image and its
original capture time; it writes no image and cannot establish new pixels.
Blank, Suspended, Stopped, discarded pixels, missing callbacks, stale times,
privacy changes and unproven geometry close the corresponding evidence.
The parent validates strict record sequence, clock mapping, PID and image
checksum before forwarding FrameMeta and structure to the host. It maintains
separate pixel-content and heartbeat leases. Receipt time cannot refresh either.

Rust now delivers PNG bytes over the independent authenticated image channel to
the readonly Operator preview; see the [image channel](../../docs/architecture/m2-image-channel.md).

This remains partial M2: MediaPipe, boot/clock identity,
physical input, hit testing, human input observation and native stop acceptance
are separate work. Platform APIs without cancellation and blocked output are
bounded by an independent process watchdog (exit 124, no partial record); the
Rust caller also terminates and reaps an overdue helper. Signals close the
stream and AX worker on their intended executor; watch stdin remains one bounded
request, not a command or input injection channel.

`self_test` generates a synthetic image entirely in memory. It verifies real
Vision recognition, coordinate conversion, sensitive-field classification,
bounded labels, wire encoding and explicit OCR overflow. It reads no screen or
target application's AX tree and does not write assets. Real target capture
requires existing macOS permissions and is validated separately.

## Owned live fixture

`fixtures/ObservationFixture.swift` is a disposable AppKit application containing
one fixed label, one checkbox and one button. It opens no user files, has no
editable or secure fields, and does not inject input. Its bundle identifier is
`com.lingxi.computer-user.m2-fixture`. Compile it into a temporary app bundle:

```sh
mkdir -p target/native-macos/M2ObservationFixture.app/Contents/MacOS
cp native/macos/fixtures/Info.plist target/native-macos/M2ObservationFixture.app/Contents/Info.plist
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors \
  -parse-as-library -target arm64-apple-macosx14.0 \
  -module-cache-path target/native-macos/module-cache \
  native/macos/fixtures/ObservationFixture.swift \
  -o target/native-macos/M2ObservationFixture.app/Contents/MacOS/M2ObservationFixture
```

Launching the fixture briefly changes the foreground application. Test only this
owned bundle with a freshly created private asset directory, then terminate the
fixture. The fixture explicitly creates its window before the event loop, joins
fullscreen spaces, and activates its own app irrespective of another app's
activation yield. The production observation helper never activates an app.
The initial AX tree can change while native titlebar controls finish loading;
the privacy comparison rejects that image and a later stable observation can
complete. Do not use fixture tests to infer permission or capture support for
unrelated applications. A successful fixture run demonstrates the current AX,
SCStream observation and Vision path; it does not satisfy full M2 acceptance.
`scripts/smoke-macos-source.mjs` additionally checks idle retains image provenance
and deliberately pauses only its owned helper to verify visible invalidation and
process cleanup. It sends no physical input.

The separate passive device guard is documented in
[native/device](../device/README.md). It does not add input APIs to this source
helper. Diagnostic hit tests and guard fault tests do not authorize native input.

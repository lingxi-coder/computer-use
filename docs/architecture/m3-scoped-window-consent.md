# Scoped window observation and consent

Native startup may specify `CU_SOURCE_READ_WINDOW_ID` and a bounded public
`CU_SOURCE_TABLE_SCOPE` JSON value alongside the registered app and fixed PID.
These options configure Source reading. They grant no Host or Device window
authority, and NativeSession still removes any startup editing permission.
Both the input JSON and serialized Source table scope are limited to 4096 UTF-8
bytes. Main rejects headers that collide under the native guard's whitespace
and lowercase normalization.

`ObservationView.scoped_window_target` is a separate read-only candidate. Its
target, public table scope, Source window context and root identity hash come
from the current globally partial snapshot's complete unique same-read census.
StateCore applies its freshness, identity, refresh and fault floors, then uses
the production dedicated WindowRoot derivation. The identity hash is the payload
of its validated `bc1:` digest. Ordinary `observed_target` remains complete-tree
gated; metadata projection never creates an Attempt, permit or input binding.

Native Source reports the exact `AX table row subtree omitted for scoped census`
Degraded reason before its intentionally partial snapshot. That reason keeps the
generic structure floor closed but does not advance the window metadata floor.
It grants no candidate by itself: the subsequent snapshot must pass the same
unique census and WindowRoot derivation. Unavailable, privacy-composite and other
AX failures still advance the metadata floor. An earlier genuine fault still
requires a strictly newer Source read.

Main accepts a scoped candidate only for the configured PID, window and table
scope. Consent compares the current app/window/title/frame, Source session and
epoch, process birth stamp, root hash and scope, with nondecreasing real read
provenance. Cancellation leaves observation enabled. Changed or expired facts,
busy tasks and uncertain cleanup prevent an authorized replacement. Old input
pipes close before new ones start, and consent does not survive product startup.

For scoped consent, Main passes the confirmed kernel process birth stamp only
to Device. Rust validates the complete stamp pair against the fixed AX window
scope; the native guard accepts it only with that fixed-window startup mode.
Guard initialization must match the live kernel stamp to the confirmed stamp.
Its existing per-action check continues to reject later PID reuse. Startup,
revocation and safe replacement options remove this grant with window authority.

The owned AppKit probe showed that attached sheets appear as direct `AXChildren`
even when `AXWindows` contains only the parent and `AXSheets` is unsupported.
Scoped activation therefore requires a successful count-copy-count read of at
most 64 direct children for every enumerated window. Each child must be an AX
element from the target process, have that window as its actual CF parent and
have a known role; Sheet, Window, Drawer and unknown roles are rejected. Counts,
types, unique references and deadlines must all agree. The retained child
reference sets must still match in the baseline inventory and both subsequent
window checks. This is a scoped activation boundary, not permission to traverse
or input into a partial row tree.

Final activation validation runs under the independent serial submission lock,
before the short state gate. Menu scanning precedes the final window inventory
and child-reference check. The state gate samples Stop, leases and the local
150 ms action deadline before consuming the intent. Actual OS calls also check
that deadline and use at most the remaining 25 ms timeout. A failure after
consumption remains Unknown and isolates input.

If a genuine Source fault clears the initial read scope, the operator may use
`Refresh observation` to start a fresh read-only lease. Main allows it only for
the fixed scoped observation policy, after the candidate is lost, with no active
Root, configuration lock, concurrent transition or uncertain cleanup. It checks
Root and cleanup again after old-pipeline shutdown and before connecting the new
pipeline. The original Source window and public table scope survive; Device
window and process grants are cleared. This is an explicit operator action.

The owned diagnostic uses an independent public TargetSpec and frozen public
reset input. It exercises the ordinary desktop permission dialog and Root
confirmation. The read-only `window-activation-submit-receipt` is captured only
from an actually matching native response through the existing bounded writer;
diagnostic loss cannot alter input eligibility or Stop. It preserves the owning
Root/Attempt/intent and actual WindowRoot context without AX handles or an
invented native call counter.

Activation evidence additionally requires the persisted `window.active@1`
effect and its original post-input Source snapshot. The production bounded
Source compiler must derive the same root identity and scope from that snapshot,
with a source sequence and observation time strictly after the native input.
This confirms only the activation phase. It does not complete the row Goal or
count toward the frozen business denominator, physical Stop latency or model
quality gates.

Fixture setup starts one independent read-only native watch for at most ten
seconds before starting the product. A successful readiness read must expose
the exact public window root, unique scope census and same-read window context.
At most 32 raw records and less than 4 MiB are archived with hashes. The helper
is terminated and its exit confirmed on success or failure. This avoids
measuring AppKit's cold window setup as product readiness and creates no input
authority. The subsequent product watch, consent and input checks remain
independent; its Source session differs from the readiness session.
The first permission dialog additionally waits for the product's own accepted
frame and available screenshot health, so it does not interrupt capture startup.
Each observation-stage recovery uses the actual product refresh button at most
once, with at most two explicit refreshes per diagnostic. All failed stages remain
recorded; refreshing does not bypass candidate or consent checks.

A terminal error after a valid native watch scope emits unavailable health with
that watch identity and a new sequence, then exits nonzero. It does not append
the one-shot unscoped ErrorResponse to the watch stream. Invalid startup parameters or scopes
and non-watch commands retain their existing error format. This preserves the
original failure reason without granting a successful observation or pixels.

Initial live failures and their exact Source snapshots are preserved in the
[current artifact](../../artifacts/m3-scoped-window-consent-ef9388a0-80f0-4c5d-916b-117937ee2637/).
Native Source root-inventory and visible-privacy failures continue to close
authority. The full original design section 26/28 acceptance scope remains open.

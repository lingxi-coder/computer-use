# Visible window privacy for scoped rows

A complete table census only establishes the requested table domain. It does
not establish that the pixels of the captured window contain no sensitive
fields. Native Source therefore needs a separate read-local audit of the
actual visible descendants of that entire window, including branches outside
the scoped table. Global `Structure.complete` remains false.

Apple defines [AXVisibleChildren](https://developer.apple.com/documentation/applicationservices/kaxvisiblechildrenattribute)
as the visible first-order subset of AXChildren. A successful bounded read of
that attribute can establish which direct children need further privacy
inspection. If it is unavailable, only a complete actual AXChildren read can
supply coverage. Table roles, AXVisibleRows, cell values and parent geometry
alone cannot establish this coverage.

The private coverage retains the process lifetime, exact captured window and
frame, frozen table scope, actual read interval and visible descendant
fingerprint. It shares the existing AX deadline and distinct-node budget.
Unknown children, duplicate references, parent/PID mismatch, missing geometry,
sensitive fields and unresolved transient layers withhold capture. All visible
descendants must be inspected recursively. This proof stays within Native
Source and never upgrades ordinary structure or focus authority.

AppKit may report a finite 0-by-0 rectangle for a scroll bar's unused increment
or decrement arrow. That is a reported empty rectangle, not missing geometry.
Only this exact standard button/subrole/actual-parent combination, with a
complete empty child enumeration, can retain its real empty rectangle in the
audit. It never supplies a hit-test or input target. Unknown children, a missing
rectangle, another parent or a nonempty descendant set still fail closed.

AppKit can return an AXGroup beneath an audited AXButton while its actual
AXParent is the captured AXWindow. The private audit records both the returned
button slot and the separately verified actual window parent. A group with
children must supply a successful full AXChildren count and exact ordered copy;
every descendant then passes the ordinary recursive audit. The same deadline
and shared distinct-node budget apply. Before the proof can finish, the group
parent and complete ordered CF reference list must match again. Empty groups
use a separate successful-zero marker. This preserves nonempty content and
does not supply ordinary structure or input authority.

One-shot capture and SCStream still require their original permission, process,
window, geometry and real timing checks. Privacy reads must precede and follow
the actual capture; SCStream keeps its actual display/callback timestamps.
Coverage or content changes invalidate the pair. Partial one-shot capture also
rechecks privacy after OCR before saving. Cached stream pixels cannot survive
an invalid periodic audit. No screenshot or timing is synthesized.

The same producer audit may establish read-only normal-window context in a
partial snapshot. Rust cross-checks the exact current provenance, unique
scoped table, actual normal-window root and frame. This public context is a
Source assertion about the window, not a serialized privacy certificate or
input permission. Generic window activation, focus, controls and UiRevisit
retain their complete-tree input requirements. A dedicated scoped activation
binding/Device path is still required before that input can be enabled.

Supervisor now accepts an explicit `sourceReadWindowId` and initial public
`tableScope` for a registered fixed process. The window policy and exact scope
are sent only to Source. Device and Host input configuration still comes from
the separate `authorizedWindowId`. The read policy requires a valid native PID
and CGWindowID; if both window options are supplied they must agree. Scope keys,
roles, UTF-8 label bounds and duplicate headers are validated before any peer
starts. This configuration supplies no Root, planner or input authorization.

The full MVP also still requires wheel authority, bounded search, rebinding
after scrolling, return-page verification and every design section 26/28 gate.
Pure tests or a source-only screenshot do not prove Host acceptance, business
completion, provider quality, physical Stop or signed installation.

The scope census now applies the same narrow actual-window parent checks to
nonempty groups and traverses their complete child lists. Because the actual
parent is the window, that branch starts without inherited scroll ancestors
from its returned button slot. It rechecks the original full list, including
empty children removed from semantic traversal. This keeps the table domain
complete when AppKit expands a group from zero to one child after capture.

Current source-only verification still withholds pixels. Six LIST-03 runs
preserve complete unique census results and zero input/provider calls. After
the parent and census fixes, the latest run reaches the unchanged-context
check and reports content or geometry changing across capture. The persistent
watch diagnostic starts real Source/Host processes, but its final observation
has no census, window context or frame. Initial scope lifetime and startup
health handling need further verification. Owned processes and channels were
cleaned up; failures remain in
[the live analysis](../../artifacts/m3-native-privacy-parent-d31cdae8-4d06-4f7e-a3ec-f180b429e756/live-analysis.json).
Native pixel publication, watch provenance and scoped startup/input are not
accepted yet.

## Capture rejection diagnostics

`CaptureReadPrivacyStatus` reports fixed categories from the same predicate used
by `captureReadPrivacyEligible`. A status never grants authority separately from
that predicate. The full-tree rule and every partial privacy requirement remain
unchanged.

`StructureRead.privacyRejection` records only a fixed category derived from
facts already read by AXReader/AXSession. It is internal to the read, survives
read-interval and captured-window reconstruction, and is absent from Structure
serialization. Successful partial coverage has no rejection tag. Process-stamp
queries retain the original boundary short-circuit; diagnostics add no AX query.

Stream revalidation reports before/after privacy status, read rejection category
and active-layer classification. It emits no target title, PID or AX identity.
The [current live record](../../artifacts/m3-capture-revalidation-tags-bfb23129-8219-4d28-9464-fbe689fe75cc/live-analysis.json)
shows eligible coverage before capture, missing coverage after, and a
`modal_or_menu_unresolved` read. The combined category does not identify the
specific modal/menu fact. Native sequence 4 was not captured, so the archive
cannot uniquely attribute every later transition. This read-only run still
failed Host frame admission; all original MVP acceptance gates remain open.

## Scoped auxiliary metadata

Registered auxiliary root metadata may coexist with exactly one intentional
scoped-row omission when the configured scope, pending census and read boundary
are valid. The existing actual CF/process/focus/modal/CG two-root proof remains
required. Other partial reasons, unknown modal values, changed roots, menus or
unstable boundaries still reject metadata.

The [current live read](../../artifacts/m3-scoped-auxiliary-privacy-a0a6938b-dc18-45f3-9233-00ac04231186/live-analysis.json)
retains the selected active layer and complete scoped census after the auxiliary
root appears. Pixel coverage remains absent: the current audit still accepts
only a single root and rejects another overlapping owned CG window. The required
continuation is a complete, shared-budget visible audit of both proved roots,
including actual CF/ordered-child postchecks and cross-read auxiliary content
comparison. Metadata proof alone does not establish absence of sensitive pixels.

## Complete dual-root audit and startup continuity

The [new checkpoint](../../artifacts/m3-auxiliary-visible-audit-c94832cc-d746-422f-8139-5eb705d34fa1/auxiliary-visible-checkpoint.json)
audits the standard root and strictly proved auxiliary root together under the
original shared 512-node budget and read deadline. Actual ordered child sources
and CF lists are re-read after both traversals; process, focus, CG inventory,
root identities, parent edges and sensitive content remain required. The
partial comparison retains both trees and fingerprints their identities and
content. Unknown or changing auxiliary content still withholds pixels.

AXSession retains selected-root CF continuity privately, latching a replacement
until session closure. A qualified post-start AX read can bind the same standard
window before first publication, with exact process/session/epoch and current
CG geometry. A monotonic Source generation may advance as an audited auxiliary
root appears; it never replaces the actual CF check. Startup clears old samples
and requires a genuine full complete sample displayed after the new read.

The failed live runs showed configuration updates replacing the auxiliary CG
window repeatedly. Startup therefore avoids those updates. Idle callbacks do
not extend the independent first-complete deadline, and cancellation is checked
before startup and around the fresh read. Before first pixels, failure reports
unavailable; after publication, existing gap/invalidation rules remain active.

Two owned read-only runs now reach Rust Source/Host admission with an actual
1160×768 image, exact unique census, 22 visible rows and matching window context.
One also preserves the verified image-channel PNG. Native Source has 255 passing
pure checks; Rust/Desktop checks are inherited with source pins unchanged.
Raw sequence/prefix omissions remain explicit in the
[live analysis](../../artifacts/m3-auxiliary-visible-audit-c94832cc-d746-422f-8139-5eb705d34fa1/live-analysis.json).
Generic observed targets remain absent, no input or model task was started,
and scoped activation/Device plus all original MVP gates remain open.

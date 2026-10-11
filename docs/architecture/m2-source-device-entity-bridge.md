# Source and Device identity proof boundary

Status: canonical-scope audit, 2026-10-05. The earlier proposal to put Source's
AX registry and tree reader into Device's helper is withdrawn. No physical
Source-to-Device CF-reference connection has been implemented or accepted.
The prior proposal and immutable validation records remain historical evidence.

## Required current behavior

The authoritative `complete-design.md` §3.1/§6.4 places AX tree reads and
notifications in Source so a slow or crashed Source does not take down Device's
Stop, guard or input-release paths. §14.8 requires Device to perform its own
current point hit test, compare the binding fingerprint's process, window,
role, label/description and geometry, and use that same local handle for AX
actions. It explicitly says that element references do not cross processes.
[Canonical process split](../design/complete-design.md#sec03) and
[canonical Stop and hit-test contract](../design/complete-design.md#sec14).

Source's `AXIdentityRegistry` assigns tokens using actual local `CFEqual`.
ReadScoped tokens expire on the next real AX read. Notification-covered tokens
may survive only under their registry/session and observer/context boundary.
Engine owns the current binding and must revoke it on the applicable identity,
read, context or global boundary change. Source tokens are value-only proof
references; they do not serialize an `AXUIElement` or grant an input permit.

Device's current hit-test path checks its own actual target and relationship
closure, rereads them locally, and passes that same worker-local AX reference
synchronously to the final submission gate. The implementation does not prove
that Device's local CF proxy is identical to a historical Source proxy. This
limit must stay explicit; a context digest, pointer string, CFHash or injected
comparison cannot establish that stronger property.

The earlier document incorrectly attributed cross-helper CF equality to the
existing full-MVP normative gate. That criterion is an additional architectural
proposal, not a requirement stated by canonical §14.8. The correction retains
all original §26/§28 task, identity/rebinding, replacement, occlusion, failure,
Stop, quality, packaging and hardware requirements. No missing acceptance
matrix is removed or represented as passing.

## Why the earlier physical merge is unsuitable

A Source traversal can use a 750ms budget, and Source's watchdog can call
`_exit`. Hosting those reads and that watchdog in the Device helper would allow
a Source failure to kill the process that owns the event tap and input guard.
Separating queues or the short Stop state lock does not preserve a process that
has exited. Device's existing worker watchdog also cannot turn a long Source
read into a safe new Device responsibility.

A stronger future design would need an explicit amendment: a separate AX
registry/action peer with Source-only read capability and Device-only one-shot
execution capability, plus an independent Device guard/Stop/held-input release
process. It would require its own bootstrap, lifetime, permissions, no-replay,
late-reply and death/cleanup validation. This is not enabled by the current
product and is not substituted for the existing process contract.

## Current acceptance work

- Keep Source namespace/generation and per-object/read-scoped invalidation,
  current Engine bindings and Device proof/deadline checks intact.
- Complete current Device context comparison for supported table columns,
  sorting, rows and local viewports. Missing local evidence must still refuse;
  partial viewport coverage cannot upgrade global Structure completeness.
- Exercise actual replacement, reparenting, reordering, duplicate labels,
  sheet/menu/window/viewport changes, occlusion, late replies and Source death.
- Complete normal/joint-load Stop and release traces, the full frozen fixture
  and TextEdit denominators, real model/resource and signed-installation gates.

The 2026-10-05 Stop submission isolation and final Unknown receipt quarantine
remain implemented; their macro-only tests prove software concurrency, not the
full physical Stop and release budget. All required live matrices and full MVP
acceptance remain open in `docs/mvp-status.md`.

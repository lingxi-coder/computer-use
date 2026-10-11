# Owned TextEdit value input

Run the explicit fresh-document diagnostic with
`CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 npm run smoke:macos-owned-value-input -- --execute`.
The metadata-only focus comparison is `npm run probe:macos-owned-focus`.

The dedicated owned profile admits one bounded AX SetValue through the ordinary
Root, Attempt, permit and execution ledger. Main issues a separate opaque,
single-use input grant for its freshly launched document. Registration alone
does not grant input. Generic keyboard and held input remain unavailable.

Native receives the immutable registration through its private startup prefix.
It accepts the four owned dependencies and their actual nullable Core validity;
missing, unknown or invalid states cannot be promoted to valid. Initial stop
sequence zero is valid, while negative values, revoked permits and stale floors
are rejected. Admission compares request times with the current source clock,
after checking clock health.

Before system-wide AX focus queries, Native inventories the unique on-screen
window matching the registered process, proof frame and window ID. A zero-input
diagnostic found that focused-application reads failed immediately on both main
and worker threads at 40 ms and 100 ms before CG window enumeration, and
succeeded after enumeration. Source already inventories CG windows first.
The additional early check keeps every existing AX focus and receiver check;
the window identity is checked again before returning the same local receiver.

TextEdit can report `attributeUnsupported` for `AXSheets` on a standard document
window. This result remains distinct from an empty list. The dedicated profile
requires exactly one AX window and two complete, bounded window-tree scans.
Every scan rejects sheets, dialogs, popovers, menus, nested AX windows, modal
nodes, unknown AX errors, malformed values and late reads. The root window must
report a typed Boolean modal value of false. Unsupported or absent modal
attributes on ordinary descendants are accepted only alongside their explicit
non-transient roles and complete child traversal. Missing or incomplete trees
cannot authorize a setter.

The receiver remains on the synchronous AX worker stack. The ordered commit
gate rechecks authority immediately before the setter. A durable `prepared`
record precedes input, and a matching terminal record follows it. Unknown
results revoke the permit and cannot replay. The independent journal auditor
requires an exact checksum-linked pair and an input span matching the Engine
effect.

The first successful editing sample is
[d6d8dec0](../../artifacts/m3-owned-value-input-d6d8dec0-e150-46a2-9a88-dce0554c199f.json).
It recorded exactly one applied input, matched a later 52-byte Unicode AX value,
and kept the Root waiting for UiRevisit. Its automatic cleanup returned unknown;
the process subsequently exited and the temporary file contained the exact
expected bytes. An independent
[cleanup recovery](../../artifacts/m3-owned-value-input-cleanup-recovery.json)
confirmed removal of that file and its private directory after checking current
process absence, recorded inode identities, permissions and stable file bytes.

That sample predates the final single-window and root-modal tightening. The
[final bdf8b3a2 sample](../../artifacts/m3-owned-value-input-bdf8b3a2-cc85-4ab4-9b20-234f419a68fc.json)
passes those gates and verifies one input, later AX value, matching journal and
complete cleanup with unchanged source and binary hashes.

Main's explicit `stopDiagnostic(expectedText)` pins an immutable expected digest.
The original stop path still rejects a replaced file inode. Only confirmed native
stop/quit receipts and observed launcher closure permit diagnostic disposal of a
replacement: the original private directory must match, contain only document.txt,
and retain its ownership and mode; the regular file must pass no-follow,
bounded stable-FD/path and exact-content checks. This cleanup cannot renew a
registration or grant input. Main's 132 tests, workspace TypeScript, Native
strict compilation and 888 counted pure checks pass. The zero-input
probe and editing sample demonstrate specific boundaries; they do not prove
controlled Save/Close/Reopen, independent revisit, live model quality, stop
performance, release packaging or the complete MVP. Save and reopen require
explicit saved-file lineage and fresh process-bound registration rather than
loosening `same_instance` or declaring CurrentUi to be UiRevisit.

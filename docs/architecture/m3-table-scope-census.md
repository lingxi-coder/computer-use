# Source-owned table scope census

Status: the typed Source interface, dynamic request lifecycle and bounded native
producer are implemented. A real LIST-03 read now returns one complete scoped
candidate with 22 visible row/cell references. Engine consumption and input
acceptance remain open; this change grants no input.

`NativeConfig.table_scope` accepts one bounded public `RowTableScope` only for
a fixed registered process/window. The request carries scroll labels and an
exact header set, with no row predicates, values, private IDs or indices.
Rust rejects unsolicited certificates or a changed scope, PID or window.
Native's production request decoder applies the same strict shape and budget.

`SourceCommand::RefreshNow { table_scope }` replaces the retained watch scope.
Engine derives it only from the frozen row goal. Ordinary goals send `None`;
pause, stop, failure, expiry and completion clear it without consuming a task
observation or starting a business refresh wait. Cleanup has a distinct
monotonic acknowledgement generation, so an old ACK cannot settle a later
business refresh. A new Host connection drops the old watch and cached frames
and restores its startup read configuration, rather than the previous task's
dynamic scope. ACK means a read was scheduled, never census or input authority.

Same-session stream rotation and the known RefreshNow invalidation transition
do not clear the scope they just installed. Session changes and accepted real
unavailability clear it; repeated fault reports do not create a restart loop.
Rejected health provenance cannot clear a current scope. Scope command tracking
is local lifecycle bookkeeping and does not duplicate Source evidence.

`StructureSnapshot.table_scope_search` is optional and same-read. Complete
zero, one and multiple table candidates mean scoped absence, uniqueness and
ambiguity respectively. Incomplete never establishes absence or uniqueness.
Rust checks actual window, table, viewport, parent path, frames and complete
headers, and rejects a complete result that omits any known matching viewport.
Global `complete` remains independent. The producer owns enumeration truth;
DTO validation cannot prove that an unreported branch was actually visited.

Native searches relevant branches before a potentially large flat-tree walk.
An exact path and actual nearest-scroll parent allow table row subtrees to be
pruned. When the census is complete, already-projected eligible tables can
also skip those subtrees in the global walk, which is explicitly marked
partial. Header mismatches remain part of the census. Unknown count/copy,
parent, alias, process/window, deadline, depth or viewport evidence stays
incomplete. The existing 512-node census/global budget, AX timeout and request
deadline remain unchanged. No-scope requests retain their ordinary behavior.

An inconsistent parent on an unrelated AXGroup is excluded only after a timely
successful AXChildren count proves it empty. Relevant scrollers, tables and
unknown/nonempty groups retain strict parent validation. For one exact candidate,
the same-read visible rows/cells and columns are shallowly projected through the
ordinary identity/privacy path, with actual process, role, parent and frame
checks. Header parent chains must reach the observed column or table. An AXButton
is eligible only as that column's actual AXHeader leaf, never as an intermediate
ancestor. Scoped sibling order stays unknown; it cannot establish an unchanged
whole-tree baseline. A failed link suppresses the viewport item references.

The [evidence directory](../../artifacts/m3-table-scope-30f642a8-70bf-4546-9520-48b0a2077773/)
retains all build, sandbox and live failures. Initial requests exposed a missing
Native whitelist entry, now fixed and covered at the actual decoder.
Subsequent reads remained incomplete at that earlier version.
An earlier traversal also exhausted its deadline. Diagnostics now preserve the
census cause rather than masking it as a window change. An unreadable table
parent cannot become a complete empty result.

The [current evidence directory](../../artifacts/m3-scope-parent-7b21bee5-c7ef-4e10-8318-b04536c55dc5/)
preserves parent mismatch, foreground loss, header-role and build/test failures.
Run `6e0185ea-3e72-4a7c-8cc5-a3b9504760e4` has a complete one-table census,
67 actual objects, 22 visible rows and cells, one column and one header reference;
the global tree is explicitly partial. Its single read took 123.509083 ms, which
is not a latency percentile or Host observation-to-input measurement. The final
wrong-header read is complete with zero candidates; the wrong-path read remains
incomplete after the global AX traversal exhausts its time budget and its final
window/process boundary cannot be certified. Both counterexamples are retained
separately. No diagnostic establishes absence or uniqueness of offscreen rows.

The probes use one production Source read per owned fixture instance. They
confirm fixture/launcher exit and stable run pins, with zero device input and
provider calls. Offline Rust checks preserve native read data and attach an
explicit synthetic diagnostic header; they establish DTO consistency only,
not Host clock/sequence acceptance. Pure tests cover the production pruning
decision helper, not full native AX traversal or hidden-table enumeration.
No scope proof was promoted into input authorization.

The [Engine scoped row consumer](m3-scoped-row-consumer.md) now validates these
references through its dedicated selection and readback paths. Native capture
privacy and startup window context still prevent claiming scoped native input.
Next work is independent privacy/window authority, bounded wheel authority,
fresh post-wheel rebinding and return-page verification. Visible-row references
remain necessary for row binding; a
viewport projection alone does not authorize selection. The 4 KiB witness
bound, original task budgets, and every canonical design section 26/28 gate
remain. Decisions API stays in external-session `llm-client` work. MVP is
incomplete.

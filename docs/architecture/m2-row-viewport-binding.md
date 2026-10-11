# Current row and viewport evidence

This implements the read-only foundation for canonical LIST-01 through LIST-04.
It does not enable row input or complete any business task. Source and Device
remain separate processes under `complete-design.md` §3.1, §6.4 and §14.8;
their AX references never cross the process boundary.

Source preserves independently observed public attributes:

| Field | Actual attribute | Domain |
| --- | --- | --- |
| `TableViewportRow.ax_index` | AXRow `AXIndex` | UInt32 global row index |
| `TableViewportCell.row_ax_index` | Singleton `AXRowIndexRange` | UInt32 global row index |
| `TableViewportCell.column_ax_index` | Singleton `AXColumnIndexRange` | 0 through 63 |
| `TableViewportColumn.ax_index` | AXColumn `AXIndex` | 0 through 63 |

Missing, malformed, merged or unsupported native index values stay unknown.
Visible array ordinals never supply missing attributes. Known contradictions
reject the affected table projection; missing indices preserve observed refs
and values. Current row/cell indices and the complete ordered visible row-index
vector enter the binding continuity digest. They are read-local relationships,
not stable business identities.

`RowSelectionObservation::from_snapshot` derives a unique scoped row from the
current Source snapshot, preserving its read ID, sequence, timestamps,
selection state, settable result and caller-supplied expiry. It reuses the
viewport-local validation without promoting global tree completeness or
granting a permit. The existing same-entity `RowSelectedEffectEvidence` requires
notification-covered table and row identity in both reads. A caller's
`same_row=true`, equal labels or repeated read-scoped tokens cannot establish
cross-read entity continuity.

`NativeRowBindingReader` retains references only inside one Device worker. It
requires an exact AXRow/AXTableRow point hit, actual table/window ownership,
complete bounded AXVisibleRows and AXColumns, unique global indices and exact
header/value conditions. Every visible row's condition cells are read through
their actual parent and singleton row/column ranges, so duplicate-name matches
cannot authorize a selected target. A direct AXCell value or its single actual
StaticText child supplies the value; secure or ambiguous values are refused.
The target must explicitly be unselected and AXSelected settable. Two reads
compare retained row, condition-cell, value-child, column and header references.
Every AX call is capped at 25ms and the reader preserves the caller's original
deadline. This reader has no setter or input entry point and is not yet used
by production permit submission.

Scalar role, subrole, parent, window and index/range attributes use the public
`AXUIElementCopyMultipleAttributeValues` API with its per-slot errors validated.
Lists retain count plus range-bounded copy, so this optimization does not fetch
an unbounded whole list. Unsupported batches refuse the read; there is no
scalar retry that renews its budget. Value reads follow structural/privacy
checks and are sampled again for the second worker-local graph.

The owned LIST-02 diagnostic observes two rows with empty action lists, actual
AXIndex 0/1, AXSelected=false and settable=true. Some blank row locations hit
the AXRow directly while text locations hit AXStaticText. This is evidence for
this layout only. EXE-07 still requires Device's current point query to return
the exact actual row before a future AX setter; lifting a descendant to an
ancestor or falling back to AXPress/click is unsupported.

The row-goal design uses a frozen semantic row target with a
stable table scope and public column conditions. GoalSpec keeps its one target
and Eq semantics: conditions can use existing ExpectedValue, with integer-only
numbers initially, and must exclude ephemeral ObjectRef, AXIndex and private
record IDs. AppPack must explicitly register the operation and effect class;
roles and labels cannot lower Unknown effects. Reaction must use ordinary
durable Attempt/permit admission and a dedicated AXSelected=true primitive and
viewport proof, preserving sticky Unknown and no replay. Fresh Source should
verify that the frozen public selector uniquely matches a selected row after
input. That current semantic state and a same-entity/causal claim remain
separate, especially for read-scoped objects.

The [row-goal integration](m3-row-goal-verification.md) now implements the frozen
public target, explicit Pack effects, confirmation projection and current
semantic Source verification. Dedicated typed permit, viewport witness and
Native setter remain unconnected. The reader additionally limits all visible
row cells to 2048 before reading their attributes or values, preserving the
existing per-row and deadline bounds.

The actual LIST-02 AXTable has no title, description or group. A future
nonrecursive `RowTableScope` can therefore name the public scroll-container
path (`Preferences content` then `Projects`) and the exact public header set
(`Name`, `Owner`, `Status`). The set is independent of current AX column order;
matching conditions keep their exact public header spelling. Each path item
uses a closed container role plus its public description. Pack registration
must bind the semantic target to this scope and the permitted operation/effect.
Resolution must prove complete enumeration of the relevant container/table
search and exactly one match, not merely one match in a truncated Source tree.
Missing descriptions, unknown/duplicate headers, incomplete search coverage or
multiple matching tables refuse resolution. The existing viewport DTO proves
local row/column coverage, not all of that container-search coverage; this
remaining admission evidence must be implemented before using this descriptor
for input. An unlabeled AXTable alone is not an authority shortcut.

LIST-03 additionally needs bounded re-search after each viewport scroll;
absence from the current viewport cannot prove absence from the whole list.
LIST-04 must rebind using invariant public columns after sorting and keep its
private record evaluator independent. All original §26/§28 denominators,
replacement/occlusion matrices, Stop, real providers, resource, recovery,
signing and hardware gates remain open.

Validation evidence is stored in
[`m2-row-viewport-8e59e648-364b-4830-80bc-dde9eb9c790f`](../../artifacts/m2-row-viewport-8e59e648-364b-4830-80bc-dde9eb9c790f).

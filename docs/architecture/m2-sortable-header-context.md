# Sortable table header context

Status: local implementation, strict builds and owned read-only comparator
checks passed; actual sorting and the full row/viewport acceptance matrix are
open.

The first table input comparator is limited to a sortable header under one
globally complete Source structure. Source records the actual `AXIndex` of
each `AXColumn`; an omitted or unavailable index stays unknown. Array position
does not supply an index. Canonical design §14.8 continues to use a Device-local
point-hit handle for the input, with Source and Device in separate processes.

A supported header has role `AXButton`, subrole `AXSortButton`, and a declared
Press action. Its actual column's `AXHeader` references that exact Source target.
The compiler checks the full ordered column projection, unique known indices in
0–63, actual column/header references, and the table in the target's ancestry.
The Source continuity digest includes the ordered actual index vector.

The existing `whole_structure_v1` witness carries nullable `table_column`:
`table_ancestry_index`, `ordered_ax_indices`, and `target_ax_index`. It carries
values for Device comparison, never a Source AX reference or permission. The
original 4096-byte witness limit remains. Other table targets and partial
`table_viewport` scopes remain unsupported.

Device reads the selected ancestor table's bounded `AXColumns` relation, actual
indices, column parents and `AXHeader` relations. It requires the correct PID
and selected AX window, a unique header that is the current point-hit target,
and the exact ordered index vector. Two reads retain and compare Device-local
table, column and header references with `CFEqual`. The original 150 ms query
deadline is shared by both reads; unknown, changed or late facts reject input.
Headers may sit above the table body's rectangle. This relation only supports
`ax_press`; it grants no click, pointer, scroll or keyboard path.

All input still requires the ordinary durable Attempt, current permits, leases,
and ordered submission gate. A read-only matched diagnostic is neither input
authority nor sorting/business completion. No cross-process `CFEqual` claim is
made. Stop and release remain independent of Source traversal.

This comparator foundation does not yet enable a normal Engine sort workflow.
The current target selector has label/group/role fields; candidate effect
classification receives role and label and leaves ordinary buttons Unknown.
Sort needs classification from its validated current table relation and must
refuse the generic pointer fallback. The current Goal predicates also have no
table-order predicate, and generic Activate verification only observes the app.
Fresh sort/table-order evidence and an appropriate verifier are still required
before reporting sorting success. LIST-02 is a same-label row-disambiguation
task used here as an owned diagnostic specimen; it is not a sort task.

Read diagnostics and local checks are recorded in
[the current validation directory](../../artifacts/m2-sort-column-e8a1571d-7035-4548-8e8e-4e1ff8356ed6).
The final checkpoint will distinguish implementation checks from actual input.
The current production Source preserves indices 0, 1 and 2 in an owned LIST-02
sample. The production Device comparator matches the current first header;
reordered columns, a different actual index, a different target column and a
missing table relation reject. An earlier reordered-column query rejected on
its 150 ms deadline; that record proves safe refusal, not the vector comparison.
The later strict-cause diagnostic supplies the comparison evidence. No input
permit, Root or Attempt was created and no physical input was posted.
The full canonical MVP matrices, row/cell/viewport comparison, replacement and
occlusion cases, physical Stop timing, live model quality and signed/hardware
acceptance remain required.

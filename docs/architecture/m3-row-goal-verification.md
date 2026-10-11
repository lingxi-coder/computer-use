# Frozen public row goals and current-state verification

`TargetSpec.row` carries a bounded `RowTargetSpec`: the actual public scroll
ancestor path, the exact public header set and exact public column-value
conditions. The scope has no Source object references, AX indices, visible-row
ordinals or private application record keys. It retains GoalSpec's single
target and Eq semantics through existing ExpectedValue; numeric conditions are
bounded integers. Header spelling is exact, and header-set matching is
independent of current AX column order. Duplicate names are rejected rather
than normalized into an alternative selector.

AppPack owns explicit row-operation registrations. The native MVP fixture has
English and Chinese profiles for saved views, projects, archive and records;
observation-only packs have no row capability, even with the fixture bundle
identifier. These profiles contain fixed public table scopes and effects, not
live row values. The registered fixture selection operation is Navigation;
dangerous label terms can raise its effect, and missing registration remains
Unknown. The bounded planning catalog publishes the fixed profiles only.

GoalCompiler accepts selected=true with UiRevisit for a registered profile,
generates RowSelected in the required predicate, and preserves the original
effect ceiling and revisit requirement. A plain selection cannot satisfy a
Commit recipe. Plan validation keeps the frozen semantic target and table
scope, and rejects substituting ordinary activation, editing or menu skills
for row selection. The confirmation projection and renderer show the public
table path, headers and exact conditions, so a same-name row can be reviewed
before authorization.

Environment's resolver searches the current active window for exactly one
table matching the actual full scroll path and header set, then finds exactly
one row satisfying all conditions. Missing labels/headers/values, incomplete
coverage, ambiguity, inconsistent ownership and invalid time bounds refuse
resolution. It derives the read-local RowSelector and RowSelectionObservation
from the same snapshot. The current implementation requires a complete global
snapshot because the Source DTO lacks a certificate for complete enumeration
of the scoped table search. This does not promote Device's viewport proof to
WholeStructure and does not remove partial/virtualized cases from the original
MVP scope; a scoped search certificate remains necessary for those cases.

Verification reads actual row selection metadata, not UiObject.value. It
re-resolves the frozen public selector against fresh usable Source evidence;
post-action reads must be strictly later than the input evidence floor. The
expiry derives from the observation and the caller's existing max age, never
from a new verification timestamp. Explicit selected=true/false gives current
semantic True/False, while missing, stale or ambiguous evidence gives Unknown.
Read-scoped references may change across reads without changing current
semantic truth. Input causality stays Unknown until dedicated same-entity
evidence is integrated; before/after selection values alone cannot prove it.

The production row input lane remains closed. `ui.select_row` is not in
Skill::IMPLEMENTED, generic Engine resolution discards a cached row-target
binding, container binding refuses row metadata, and computer fallback cannot
drop row conditions into a generic activation. No RowSelected predicate grants
a permit or invokes Native input.

The remaining execution closure needs a dedicated TableViewportV1 witness,
RowSelection observation purpose and SetSelectedTrue action. Source proof and
the value-only witness must share one current snapshot and bind actual ordered
row/column indices, selector cells, false/settable selection state, geometry and
the current point. Native must independently query that exact AXRow/AXTableRow
point and retain two local graphs through the original deadline. The reader
now caps all visible-row cells at 2048 as well as 128 per row; count/range reads
must refuse excess before reading cell values. Production still needs required
expected row/column order and the bounded table-to-scroll geometry/path check.

Core must limit this purpose to one zero-offset action, consume the row permit
once, and preserve ordinary durable Attempt/intent/journal and Stop admission.
Only after revalidation may the Stop submission gate invoke one AXSelected=true
setter on the local point-hit row. A descendant hit, uncertain completion or
expired relationship cannot fall back to AXPress/click or replay. A fresh Source
read verifies semantics separately from notification-covered same-entity
effect evidence. These are implementation requirements, not completed input
or business acceptance.

LIST-03 viewport re-search, LIST-04 sorting and row editing, the original
fixture 100 and TextEdit 20 denominators, real providers, Stop, recovery,
resources, signing and hardware gates remain open. Current checks and failed
attempts are retained in
[`m3-row-goal-2220c0e5-1f37-4e6d-9e04-d02afb00f88d`](../../artifacts/m3-row-goal-2220c0e5-1f37-4e6d-9e04-d02afb00f88d).

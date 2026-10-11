# Frozen-row viewport search

Status: plan validation, typed Source census, frozen-goal request lifecycle and
[real scoped row references](m3-table-scope-census.md) and the
[Engine scoped consumer](m3-scoped-row-consumer.md) are implemented. Native
privacy/window authority, scoped wheel authority and the runtime search route remain
required work.
This document does not enable partial Source input.

## Current evidence

Two [production Source reads](../../artifacts/m3-row-search-5ca51508-3a0a-4af0-a139-11c2571236e0/list03-source-scope-analysis.json)
of owned LIST-03 fixtures produced a globally incomplete 512-object tree:
`AX child list truncated; AX node budget exceeded`. The same reads contain a
complete column projection and 22 complete visible rows, AXIndex 0 through 21.
The public scope is 偏好设置内容 → 归档, with column header 归档项目. Target 101
is outside that initial viewport. Both scrollers report value 0 without min/max.
The fixture and launcher exits are confirmed; target input and provider calls
were zero. No fixture snapshot was used for target selection.

The first source-only driver record retains its failed overall status because
its original status aggregator did not recognize the newly added mode. Its raw
Source capture remains evidence of partial structure. The second record has
the corrected mode status and stable pins. Neither is a business run.

## Implemented plan constraints

For a row Goal, every `ui.scroll_to` must explicitly name the frozen row
semantic target and its registered nearest scroll container. Each scroll must
be a transitive prerequisite of the unique `ui.select_row`. Every target
revisit remains a dependent of that selection. Planner vector order cannot
substitute for dependency edges. A visible row still permits the existing
selection/revisit plan without scrolling. Neither Goal conditions nor budgets
change. Tests also keep the wrong-scope dependency graph valid so an ordering
error cannot mask a scope validation regression.

## Source scope evidence to implement

The request should carry the exact frozen public `RowTableScope`: all scroll
ancestors and the complete header set. It must not carry row predicate values,
private record IDs, AXIndex or coordinates. Host retains the full frozen
`RowTargetSpec` and applies its predicates locally to each new Source read.

The response needs a typed same-read table census, separate from global
`StructureSnapshot.complete`. It must bind the requested scope, current
Source identity and clock stamps, application/process/active window, actual
ancestor paths and all matching table candidates to that read. Complete
coverage and exactly one matching table can identify the current table and
nearest scroll container; absent, ambiguous and incomplete are distinct
outcomes. Candidate references must cross-link to the actual same-read
TableViewport and objects. Unknown labels, headers, parent relationships,
count/copy mismatches, timeouts and omitted relevant branches make the census
incomplete. No traversal or TTL bound increases are justified by this fixture.

The census must avoid expanding the selected virtual table's row data as a
global tree. Pruning must be justified by the exact path and direct table
parent semantics: blindly treating every unrelated table as a leaf could hide
a still-eligible nested scroll scope. A branch that could still contain the
requested scope must be searched within the existing bound or reported
incomplete. Pure tests must cover this case and a second matching table hidden
after the first table's large row subtree.

This proves current table-scope coverage, not stability across reads or
whole-list row uniqueness. Existing row contracts identify a currently visible
row. Complete visible rows do not prove that an unseen duplicate is absent.
Whole-list uniqueness/absence requires separately sufficient actual row-count,
index coverage and boundary evidence; no end can be inferred from a missing
min/max or from exhausted actions.

## Runtime and Native work to implement

Engine must consume the same scoped evidence in its row resolver, fresh row
binding, row-selection observation and Root verifier. It must keep incomplete
global Source away from generic Binding/TargetInput. Every newer read replaces
table, container and row references; the original frozen conditions survive.

Wheel input needs its own action-specific table-scroll witness and purpose.
Selection-only TableViewportV1 must continue to authorize only
`AXSelected=true`. Native must independently reread the bound active window,
path, table, nearest scroll container and viewport, and retain its existing
Stop, pointer-center and ±120px wheel gates. Pointer preparation consumes
budget. Every known wheel completion requires a strictly later scoped Source
read. Unknown cannot trigger another wheel or a selection replay.

Search consumes a fixed admitted quota and the original Root deadline; a
planner/replan cannot increase its action allowance. The current 16 × 120px
ceiling does not establish full coverage of this 4402px table with a 500px
viewport. Exhaustion or no provable edge remains incomplete. After returning
from Overview, the rebuilt viewport must be searched/rebound or have separate
sufficient selected-row readback evidence; a restored fixture selection alone
does not complete the stronger Root goal. Actual 22-row input witnesses must
also satisfy the unchanged 4 KiB wire budget before an action is enabled.

Required regression coverage includes old/new Source namespaces, stale scoped
replies, incomplete or ambiguous table census, hidden matching branches,
viewport rotation/reused refs, missing ranges, no progress, quota exhaustion,
Unknown and cancelled wheels, window/geometry changes, row selection without
replay and fresh return-page Root verification. Real scope reads and inputs
must be preserved separately from pure models and reference-planner samples.

The full canonical §26/§28 denominators, cold knowledge, real model quality,
fault/Stop/latency, resources, signed packaging and second machine gates remain
open. Decisions API is still external-session work in `llm-client`; the product
dependency pin remains unchanged. The first MVP is incomplete.

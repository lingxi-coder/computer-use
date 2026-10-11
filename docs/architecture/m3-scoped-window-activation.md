# Scoped window activation

`WindowRootV1` is a purpose-bound proof for activating the fixed authorized
window when an exact frozen row scope has a complete unique same-read census
inside a globally partial AX snapshot. It carries comparable app, normal window,
root and Source process/window provenance. It carries no AX handles or sibling
closure and cannot authorize ordinary controls, row selection, wheel or keys.

The registered Root flow reads the table scope from its frozen `GoalSpec`, binds
the current root through `Environment.BindingReducer`, admits one registered
`ui.activate_window` attempt and uses the existing durable decision mapping,
permit and Execution revalidation. Complete snapshots keep the existing
`WholeStructureV1` activation path. Missing row scope, census ambiguity, unknown
title, unsafe layers, stale evidence and mismatched scope reject the new path.

Execution rederives the purpose-specific proof from current Source facts. Its
closure commits stable root and scope identity while current source sequence
and observation time remain independently checked. A notification-covered
root can retain its identity across fresh reads; a read-scoped root cannot.
Activation observations require verified Source provenance, exact authorization
and window dependency versions, and at most a two-second lifetime.

The native Device independently reads the actual process birth stamp, AXWindows,
the unique standard normal root, known title/frame/modal values, explicitly
empty AXSheets and matching layer-zero CG window. A single auxiliary AXDialog
requires a proved actual CF parent relation and matching inventory. Both reads
retain and compare actual CF objects and their facts within the original 150ms
query deadline. Menu traversal sets the bounded messaging timeout on every
actual node before each synchronous query. Missing or changing facts reject
before the guarded native activation commit.

A partial snapshot reporting the window as active still goes through Device
activation. After a known completion, the attempt waits for a strictly newer
Source observation and sequence beyond the input boundary with the same frozen
scope and fixed window. Unknown completion stays unresolved and is never
replayed. A receipt establishes input completion; task success still requires
the original effect verification.

The replay fixture comes from a pinned successful read-only Source/Host run;
it is historical test input, never current live authority. Pure tests, mocked
Stop concurrency and offline builds do not establish native activation,
business success, physical Stop latency or model quality. The original design
section 26/28 acceptance gates remain required.

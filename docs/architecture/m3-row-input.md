# Dedicated row selection input

The registered fixture row Goal now has a separate local execution path:

`FrozenGoal → current Source row resolution → RowBinding → Reserve/ActivateAttempt → RowSelection Observation → single-use permit → AXSelected=true → newer Source semantic verification`.

The Goal retains its public scroll ancestry, exact header set and exact column
conditions. An optional group remains a constraint. Source object references,
AXIndex and coordinates are generated from current evidence; they are not Goal
fields or model-supplied action parameters. Admission can re-resolve the same
frozen selector after a pending decision's Source references rotate, preserving
the original decision identity, Root epoch and deadline.

RowBinding stays separate from the generic control Binding. `ui.select_row`
requires a currently unselected row with a known settable selection attribute.
An already selected row produces no input. Ordinary AXPress, pointer actions,
generic TargetInput and computer fallback cannot consume this row proof.

The TableViewportV1 witness commits the actual ordered column indices, headers
and frames, all visible rows' indices, frames and relevant selector values,
window/table/viewport geometry, raw public scroll descriptions, normalized
target descriptor and group, selection capability, and one candidate point.
Its 4 KiB limit remains. The point lies in the row/viewport/window intersection;
the Source geometry does not claim an actual point hit.

ExecutionCore grants at most one row permit per Source read in its session and
surface namespace. Changing the evidence ID cannot mint another permit. The
submitted intent must use that permit's exact evidence, purpose and complete
proof. Renewals, later Source reads and different intents are refused; repeating
the same accepted intent remains receipt-idempotent. These rules apply to both
ReadScoped and NotificationCovered row evidence.

Native uses the existing fixed launch authorization and Stop submit gate. A
system-wide point query must directly return the expected current AXRow. Two
bounded worker-local AX relationship reads must agree, followed by another
point query that still returns that same local row, table and selected active
window. AXIndex is read from AX and is independent of Source sibling position.
The original 150 ms query budget, 25 ms per AX call, 128 cells per row and 2048
total cells remain. Local AX handles do not cross processes. The surrounding
Host proof references are opaque immutable commitments, not Native handles.

The submit fragment calls exactly one
`AXUIElementSetAttributeValue(row, AXSelected, true)` after the ordered Stop gate.
An uncertain result cannot be replayed. Mechanical completion only permits
read-only result verification: a strictly newer Source read must uniquely
resolve the frozen selector and observe selected=true. This does not promote
input causality. The row Goal remains intact through `ui.revisit`.

The trusted fixture reference planner accepts the explicit diagnostic prefix
`Select fixture row: ` followed by strict public TargetSpec JSON. It emits only
`ui.select_row → ui.revisit`, under the registered Navigation effect. It is a
diagnostic path through ordinary Root/Attempt machinery and does not count as
S1/S2 model acceptance.

## Verification boundary

The current local checks and source/binary pins are recorded in
[the row-input artifacts](../../artifacts/m3-row-input-fab1c81d-7b36-423d-b78d-598efa29d0f3).
Local checks cover contract/Core/Engine logic and Native pure wire checks. The
Native relationship preflight compiled, without a new live AX row probe.
Simulated execution remains separate from actual input and business acceptance.
No actual row input, provider call or Native row Root/Attempt was performed in
this implementation run.

The Engine resolver still requires global complete Source coverage until a
proper scoped search certificate exists. Virtualized/offscreen search, row
scroll recovery, sorting/editing relationships and a real ordinary fixture
Root/Attempt run remain open. All original canonical design §26/§28 business,
model, Stop, recovery, resource, packaging and hardware gates remain required.

## Project rename

The primary checkout is now `/Users/luolingfeng/lingxi/computer-use`; repository
`lingxi-coder/computer-use` retained GitHub repository ID 1396765899. `origin`
uses `git@github.com-lingxi-coder:lingxi-coder/computer-use.git`. npm workspace
packages and visible branding use computer-use / Computer Use. Existing
`com.lingxi.computer-user.*` Bundle IDs and Keychain service identities remain
stable. Historical artifacts keep their original text and absolute paths;
[the relocation record](../../artifacts/m3-row-input-fab1c81d-7b36-423d-b78d-598efa29d0f3/rename-project.json)
maps the old checkout to the new one. No commit or push was performed.

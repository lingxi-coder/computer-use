# Scoped row evidence in Engine

Engine can now consume a globally partial Source snapshot for the frozen
row-selection domain. `row_scope_coverage` validates the same-read snapshot and
an exact complete table census. Resolution searches only its candidate tables;
zero candidates or no visible matching row remain incomplete search, and
multiple matching tables remain ambiguous. Neither result proves absence or
uniqueness across offscreen rows.

`RowBinding` retains the frozen public table scope. Binding, the dedicated
DeviceView row observation, SelectRow and row-effect verification recheck it,
the candidate/table identity, and actual row/cell/column/header references.
The input remains one AXSelected=true operation through the existing witness,
admission, permit and Stop gates. Selected=true permits zero input; false or
unknown readback after the one input does not authorize replay. Effect readback
must be strictly later than the input boundary and have a later Source sequence.

Environment owns a separate row freshness floor. A valid complete scoped census
may explain AX Degraded on a partial tree; it cannot keep an older whole-tree
snapshot alive after Degraded. Unavailable, refresh, session/geometry changes,
pixel expiry and execution floors remain closed until genuinely new evidence
arrives. The dedicated row frame lease uses the actual FrameTracker content,
heartbeat, screenshot health and window geometry. It does not invent pixels.

The generic structure/frame gate, ordinary controls, keyboard, menus and
UiRevisit navigation keep their complete-tree requirements. The Root exception
is limited to its frozen SelectRow step; it does not enable arbitrary partial
input or relax root completion.

Native scoped input remains unverified. Current partial reads withhold pixels
under the existing privacy rule, and startup activation still needs a complete
window context. The [next native prerequisites](../../artifacts/m3-scoped-row-consumer-6b50ee37-fd6a-4869-bfcc-50108592b662/source-privacy-next.md)
require independent full-window visible privacy coverage and window authority
without promoting the global tree. Wheel authority, post-wheel rebinding,
return-page verification and every canonical design section 26/28 MVP gate stay
open. Decisions API remains external-session llm-client work.

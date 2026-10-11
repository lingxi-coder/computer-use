# Native owned-document Save

Save is a separate, default-off action for the Main-created private TextEdit
plain-text document. Main issues an opaque, single-start Save authorization in
addition to the value-input authorization for the same registration. Product
startup and NativeSession replacement discard both grants; the existing input
consent does not create a Save grant. The local diagnostic opts into Save
explicitly with `--execute --save`.

The typed primitive is `SaveOwnedDocument { expected_value }`; it carries the
frozen desired text within the existing 2048-byte/NUL-free policy. The dedicated
observation purpose is `OwnedDocumentSave`. Generic bindings cannot authorize
Save, and a value-setter observation cannot authorize a Save primitive. Core
keeps the exact four dependencies: authorization, focus, window and owned scope.
The setter-only profile continues to reject Save.

The native worker independently verifies the owned process, focused document
and receiver, current exact AXValue, bounded window tree and deadline. Its Save
selector checks the complete direct File menu path and exactly one enabled
Save item with Command-S metadata and AXPress support. It rechecks the same
item and document immediately before the ordered commit. No keyboard fallback
is available. Prepared and terminal journal records carry the same fixed action
identity, so the independent auditor can distinguish a setter from Save and
verify both pairs in order on one checksum chain.

The Engine uses normal candidates, durable Attempt reservation/mapping/outbox
and execution permissions. The explicit reference plan `Fill and save owned
document: <text>` advances from a verified setter to one Save attempt. A matching
completed Save receipt proves an input span only. It cannot produce a saved-file
EffectResult or complete the Root. Unknown input cannot replay Save.

The diagnostic independently checks private file bytes before closing, then
retains the private file and launches a new owned process/registration. Fresh
reopened AX reads test the visible text. These checks do not yet provide the
retained Root's UiRevisit evidence: the current diagnostic closes the first
Supervisor and starts another in observe mode. Root continuity and the trusted
saved-file/revisit evidence bridge remain required before business acceptance.

The [712ca06b live diagnostic](../../artifacts/m3-owned-edit-save-reopen-712ca06b-3758-4f4e-bac1-42252765ffe5.json)
verifies one actual setter and one actual Save through two normal Attempts, four
exact journal records, 52 expected UTF-8 bytes before closing and three fresh
exact reopened AX reads. Root remains waiting for evidence; both SDK ledgers
stay at zero. Cleanup succeeds, no peer exits unexpectedly, and binary/source
hashes remain unchanged. This is one isolated execution diagnostic, not the
TextEdit business suite or retained Root revisit acceptance.

Current checks are contracts 118, Core 106, Engine 383, Host 43, Device 58,
Source 64, Main 146, desktop-api 12 and journal 7; Native strict compilation,
888 counted checks and eight additional mounted suites pass. Clippy, format,
TypeScript and schema checks also pass. No live-provider call, signed-package
or full MVP acceptance is implied.

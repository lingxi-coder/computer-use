# M2 AX object identity coverage

Status: mandatory contracts, native and Rust Source coverage, Engine selective invalidation, and Device proof admission are implemented. A read-only native diagnostic confirms stable covered identities and fresh read-scoped identities in FORM-02 and LIST-02. A bounded Device relationship witness and its fresh native comparator are implemented; ordinary-window read-only checks have passed. SortButton, specialized viewport execution, and the complete acceptance matrix below remain open. Cross-helper CF identity is a separate optional architectural strengthening, not an existing canonical §14.8 requirement; the withdrawn shared-Device-helper proposal and its scope audit are recorded in `m2-source-device-entity-bridge.md`. This decision does not reduce or replace the canonical design or its frozen task set.

## Implemented checkpoint


The [bounded-witness checkpoint](../../artifacts/m2-device-context-witness-checkpoint-f12aed78-daa2-4e53-a8ba-2106def1dff4.json) records the current compiled package and checks. The [actual native diagnostic](../../artifacts/m5-native-context-3e92fb3f-08cb-4beb-a251-b8bed75661f9/native-context-3e92fb3f-08cb-4beb-a251-b8bed75661f9.json) took exactly one complete Source read per FORM-02/LIST-02 case. Both selected targets were unlabelled ordinary-window AXButtons; no sheet was observed. Both positive probes were eligible, and changing one non-target sibling label made both probes context-unresolved. All four replies remained input-unauthorized, all owned processes exited, and source/binary pins stayed stable. Witnesses were below 4 KiB and the complete guard records below 8 KiB. The earlier runs, including the first successful binary snapshot, and a separate cleanup audit remain preserved.

These direct probes use diagnostic snapshot/sequence headers and stricter read-scoped proofs; they do not claim Host clock mapping or production Source admission. Timely NoValue/Unsupported label reads may produce an empty label, matching the existing Source/fingerprint contract. Failed, mistyped, oversized and late reads still refuse. Native nullable fields encode explicit nulls. The witness window title comes from the actual AX root title, while bc1 retains Source window metadata. Menu/item top-level requirements remain mandatory; intermediate ancestors retain their complete parent/child and worker-local graph checks. Actual menu/sheet/scroll/replacement and input matrices remain open.

The [native read audit](../../artifacts/m2-native-identity-readonly-audit-01faad2c-5754-44b3-a183-3fb83ec0616a.json) records four complete reads per case. In the last two reads of the same generation, FORM-02 retained all 15 covered refs and LIST-02 all 27. Their six and ten read-scoped refs respectively had zero cross-read overlap. Observer health honestly stayed `degraded`; actual unsupported Destroyed registrations remained visible as `-25207`. One auxiliary AX window appeared, causing a legitimate generation change; the following generation and tree revision stabilized. All owned processes exited. This was zero-input, zero-provider observation, not business or notification-delivery acceptance.

Source assigns continuity from each object's own required registrations. Binding compilation independently checks its actual ancestry, group, direct sibling order and table relations; a read-scoped member of this relevant closure makes the binding read-scoped. An unrelated unsupported title-bar node does not recursively downgrade every control. Context digests exclude ordinary target values and focus effect fields, while retaining fields needed to distinguish the target.

The Rust watch tracker validates counts and failure refs and rejects read-scoped token reuse across distinct native reads. Its bounded history covers eight reads, at most 4,096 tokens. Frame publication may repeat one AX read under a newer public Source sequence; this is not a new native read. Engine nevertheless expires a read-scoped binding on the next public Source sequence. One-shot Source has no observer continuity and publishes all objects read-scoped with `unavailable` coverage.

Engine correlates Observe and Grant replies with the exact attempt and observation. Dropped bindings cancel their dependent decisions and revoke their associated permits; a late Grant is revoked rather than transferred to another dispatch. Timed-out Grant correlations are bounded to 1,024, with oldest-first eviction. Wrong reply variants preserve the expected correlation; even an unknown or evicted correlation carrying a Granted permit emits an exact Revoke. Global boundary changes and unavailable generic Source remain fail-closed. A separately proved owned-document receiver keeps its own lifecycle and expires independently.

Rust Device/Core link the immutable issued evidence to the admitted proof. Read-scoped permits cannot migrate to a new read; covered proofs must preserve the closure. The same Rust compiler derives continuity and a mandatory `device_context` witness from the actual Source closure. The whole-structure witness includes precise target/window frames, ordered ancestry and direct siblings, the nearest labelled group, and every scroll ancestor. Focus is added only through the explicit focused-control derivation; ordinary target input does not inherit focus authority. Actual menu operations retain their separately validated anchor/menu relation.

The native request carries one current `context_witness`, capped at 4,096 encoded bytes, while the complete guard record remains capped at 8,192 bytes. Original/current native proofs omit the duplicate witness. Native Device parses the strict witness and reads current worker-local AX relationships before commit, retaining CF handles to compare the target, path and ordered sibling graph across its reads. It compares precise frames and relevant descriptors; it does not recompute `bc1`, which also commits opaque Source refs. Unsupported `table_viewport` witnesses remain non-executable.

Source opaque ObjectRefs still have no mapping to Device's local AXUIElement handles. A matching descriptive witness and CFEqual checks inside the Device worker do not prove that its handle is the original Source entity. That stronger cross-helper claim has not been established or enabled. Canonical §14.8 instead requires Device-local hit testing and fingerprint validation without cross-process element references. Actual relation/replacement acceptance and the atomic SortButton or row-selection paths remain required and open. Viewport execution additionally needs an actual table/scroll/visible-row/column/cell relationship read and current AXSelected settable evidence; neither `bc1` nor labels can supply these facts. Direct native diagnostics now use the same Rust closure compiler, with explicitly read-scoped diagnostic proofs and no Host clock or input authority claim.

## Decision

Treat tree traversal completeness, observer coverage, object continuity, and the global Source identity boundary as separate evidence. A locally unsupported AX notification must not force every object into a fabricated persistent identity, and must not silently erase the rest of the tree’s usable evidence. An object without enough notification coverage is read-scoped: its `instance_ref` changes on every Source read, regardless of `CFEqual`. A persistent binding is permitted only when the target and the context needed to identify and execute it have adequate continuity evidence.

The boundary in `StructureSnapshot.identity_boundary` remains a global registry/session reset marker. It changes when Source cannot establish global process/root/observer continuity or explicitly resets its registry. It does not change merely because one object is read-scoped, a fresh read occurred, or tree content changed. Existing global invalidation remains the safe response to a true global boundary change.

## Earlier evidence and whole-registry fallback

Current live macOS fixture observations reported `kAXUIElementDestroyedNotification` registration unsupported on modal title-bar `AXStaticText`, the form’s internal scrollbar proxy, and list `AXCell`, `AXSortButton`, and title `AXStaticText` nodes. The same run reported support on `AXRow`, `AXTable`, `AXColumn`, and ordinary NSView-backed buttons; an actual `AXRow` exposed settable `AXSelected`. These are observed role/class results, not hard-coded assumptions about all applications. The related live evidence artifacts are:

- [`m5-native-source-modal-table-checkpoint-1c1556ff-e5e8-43a4-bf02-c8edb69ce59c.json`](../../artifacts/m5-native-source-modal-table-checkpoint-1c1556ff-e5e8-43a4-bf02-c8edb69ce59c.json)
- [`m5-native-source-modal-table-checkpoint-c3f8f5c7-3b91-4ec3-8551-b97603732604.json`](../../artifacts/m5-native-source-modal-table-checkpoint-c3f8f5c7-3b91-4ec3-8551-b97603732604.json)
- [`m5-fixture-source-diagnostic-e8301a82-d858-4e7a-92d7-95247349e25e.json`](../../artifacts/m5-fixture-source-diagnostic-e8301a82-d858-4e7a-92d7-95247349e25e.json)
- Foreground fixture Source reads: [`38b44807`](../../artifacts/m5-fixture-foreground-source-38b44807-44cf-439b-991e-459224da5c72.json), [`5dd9ba42`](../../artifacts/m5-fixture-foreground-source-5dd9ba42-9623-4b90-811c-3c515d93ffa3.json), [`657b5b07`](../../artifacts/m5-fixture-foreground-source-657b5b07-950e-45ed-8801-424126c879bb.json), [`76cd9520`](../../artifacts/m5-fixture-foreground-source-76cd9520-6b39-4d6a-bc5e-cb8331df0782.json), [`c6b23c20`](../../artifacts/m5-fixture-foreground-source-c6b23c20-6e9d-4092-b5a4-895c713106d6.json), [`e3e1f850`](../../artifacts/m5-fixture-foreground-source-e3e1f850-6fba-47cf-8af0-cb2273ceadec.json), and [`e47595c6`](../../artifacts/m5-fixture-foreground-source-e47595c6-01bd-4b02-934a-d5ef5a0e0c33.json).

Before this checkpoint, the implementation deliberately failed closed at whole-registry scope:

1. `AXSessionWorker.identity` registers Destroyed as required for every inserted node; windows also require moved/resized registrations. `register` records a required failure in the single `registrationFailed` flag.
2. That flag makes `observerAvailable` false and prevents `continuityVerified` from being restored. At the next read, `continuityLostBeforeRead` prevents candidate reuse; `invalidate()` clears the registry and increments its generation before rebuilding the observer. Encountering the same unsupported nodes can repeat this cycle.
3. A failed candidate revalidation can also make that read incomplete. An otherwise complete traversal may still produce a tree snapshot with degraded observer status, but a later registry generation change reaches `identity_boundary` and invalidates all old target authority.
4. `EvidenceCoordinator` treats a changed or missing established identity boundary as a forced target reset. `BindingReducer` then loses its old bindings; pending decisions and device permits depending on the old target authority cannot remain usable.

That conservative fallback exposed the implementation gap addressed here. Required lifetime registrations remain required for persistent identity. Unsupported nodes now receive fresh read-scoped refs; they do not retain IDs through `required: false`.

Code references: [`AXSession.swift`](../../native/macos/AXSession.swift) (`AXIdentityRegistry`, `register`, `ensureObserver`, `identity`, `TreeContext`, `identityBoundary`, `read`); [`Accessibility.swift`](../../native/macos/Accessibility.swift) (`AXReader.visit` and traversal completeness); [`source.rs`](../../crates/interaction-contracts/src/desktop/source.rs) (`UiObject`, `StructureSnapshot`, `SourceIdentityBoundary`, `TableViewport`); [`tracker.rs`](../../crates/source-macos/src/native/watch/tracker.rs); [`binding.rs`](../../crates/interaction-engine/src/environment/binding.rs); [`state_core.rs`](../../crates/interaction-engine/src/environment/state_core.rs); [`evidence.rs`](../../crates/interaction-engine/src/engine/evidence.rs); [`authority.rs`](../../crates/device-macos/src/guard/authority.rs); [`AtomicSubmission.swift`](../../native/device/AtomicSubmission.swift).

## Continuity model

Every projected `UiObject` must carry an explicit, validated continuity mode. Missing continuity metadata is invalid; there is no default to persistent identity.

| Mode | Source claim | Binding lifetime |
|---|---|---|
| `notification_covered` | This object's own required lifetime/context registrations succeeded; Binding compilation separately checks the relevant closure. | May survive a new read only if the same Source object token, global boundary, covered context proof, active layer, and action-relevant fields still match. |
| `read_scoped` | This read observed an object and its current descriptors, but cannot prove that the next read will expose the same underlying UI object. | Valid only for its `snapshot_id/source_sequence`. A later read makes the old binding unusable and requires a fresh binding/decision. |

The Source-wide observer state is independently `available`, `degraded`, or `unavailable`. `degraded` means some objects or notification classes lack coverage; it is not a statement that every observation is false or that the whole tree is incomplete. `unavailable` means Source cannot establish the app/process/root channels needed to scope trustworthy observations. `StructureSnapshot.complete` continues to mean bounded traversal completeness only.

For `read_scoped` objects, mint a new opaque `instance_ref` for every Source read even if the AX proxy compares equal. `CFEqual` may still resolve relationships within one read. It must not be used to claim cross-read lifetime when Destroyed coverage is absent. For covered objects, registry reuse still requires a live notification registration and a stable relevant context; an observer registration failure downgrades only the affected object/closure unless it affects the global root channel.

The minimum contract additions are:

- `UiObject.identity_continuity`: mandatory enum `notification_covered | read_scoped`.
- `StructureSnapshot.observer_coverage`: bounded state plus counts for covered, read-scoped, and failed registrations, with bounded reason/notification codes. Do not include labels, values, raw AX references, or unbounded platform error text.
- `BindingProof.continuity`: mode, originating source sequence, and a digest of the exact context closure used for this binding. Add a proof scope `whole_structure | table_viewport`; the viewport form identifies its table, scroll container, current viewport and local completeness evidence.

These fields are mandatory in the current contract. Generated schemas, fixtures, TypeScript types and Source/Device readers were updated together; missing metadata is invalid rather than defaulted to covered.

## Context closure and invalidation

Continuity is a property of a binding’s complete proof, not only its leaf object. The closure includes the actual ancestry to the selected window; parent-child membership and sibling order when used by the fingerprint; the nearest labelled group and other semantic context used to disambiguate; and the table, column, row and scroll-container relations for list actions. A relevant closure node with unsupported or failed coverage makes a cross-read binding read-scoped. Do not infer that `AXUIElementDestroyed` alone detects sibling reorder, label change, or parent-child changes; use an applicable registered change notification or require a fresh observation/current-context check.

Route notifications according to their scope:

- A known object destruction invalidates that object and proofs depending on it. A scoped layout/content event invalidates only the context closures that depend on that element, unless the event cannot be mapped uniquely.
- Process identity loss, selected root/window inventory uncertainty, observer teardown, unmapped invalidating events, or registry corruption resets the global identity epoch and invokes the current whole-generation fail-closed path.
- Value-only updates do not automatically imply a new object identity. If the value participates in the pending decision or intended effect, that decision must still be revalidated against the new observation.
- A tree topology change updates tree evidence and affected context digests; it does not by itself manufacture a new global identity boundary.

The global `identity_boundary` continues to close all target authority on change. A partial coverage report must not increment it. `AxObserver: degraded` remains visible to the operator and must not silently reopen stale permissions; the Engine admits each binding from its object/context evidence. A genuinely unavailable Source root channel blocks new target authority until a fresh verified baseline is established.

## Binding, decisions, and permits

`BindingReducer` should retain a covered binding only when its exact object token, global boundary, context-closure digest, layer/page and fingerprint all revalidate. A read-scoped binding expires on the next Source sequence. The reducer must return the invalidated `BindingRef`; Engine revokes every permit and pending decision that depends on it before accepting another target action. Rebinding after a new read creates a new binding reference from current evidence. It is not migration of the old reference, old decision, or old permit.

A fresh read scoped to one object must not clear unrelated bindings whose covered closures remain valid. Conversely, a global boundary change retains today’s conservative whole-target reset. Pending decisions tied to a read-scoped binding close on a new read; they cannot be carried onto a replacement token solely because label, role, and frame happen to match. A permit is valid only for its fixed source sequence or the same covered-object proof; cancellation, observer uncertainty, context change, or a newer read revokes the affected permit. For an already dispatched input whose outcome is unknown, reconcile the original intent and never submit it a second time.

## `AXSortButton`: read-scoped single action

`AXSortButton` is a real executable target in the frozen list workflow but was observed without Destroyed notification support. It must remain in MVP scope as `read_scoped`; it must not be promoted to `notification_covered` by comparing `CFEqual` or an unchanged fingerprint.

The permitted path is one fresh, atomic action:

1. Source observes the actual button in a current table viewport and assigns a new per-read token. It proves a unique button-to-table/column relation using the actual column/row/cell ranges and labels available in that read; ambiguity, missing relationships, or an off-viewport button produces no executable proof.
2. Engine creates a new one-shot binding and decision from that snapshot. The proof carries the snapshot/source sequence, table scope and context digest. It does not reuse an earlier binding or permit.
3. Device re-reads the current app/window/table context inside the ordered submit path, resolves exactly one matching current SortButton for the same table/column operation, checks active layer and visibility, then immediately performs one action on the just-read AX handle before yielding the worker. The action is refused if the worker cannot make this current proof and action one bounded atomic operation.
4. Afterward, Source observes the reordered state. The new read receives new read-scoped SortButton identity and all affected bindings are re-evaluated. Unknown submission remains unknown and is never replayed.

This path is action-specific. It does not establish general persistent identity for unsupported objects or authorize an arbitrary action by label/geometry alone.

## LIST-03 local viewport while the whole tree is incomplete

The existing `TableViewport` projection already separates viewport-local completeness from `StructureSnapshot.complete`. Preserve that separation. A read with an incomplete unrelated subtree remains globally incomplete and cannot use generic whole-tree binding rules. A specialized list operation may use a viewport proof only when the exact current `AXTable` and `AXScrollArea` are present, `visible_rows_complete` and required column/cell evidence are true, rows are actual visible `AXRow` objects, selected state and settable status are directly observed where needed, and all relevant parent/range/frame/process relationships validate.

The viewport proof authorizes only the operation whose target and evidence are inside that exact current viewport (for example, selecting or updating a uniquely resolved visible row). It cannot make unseen rows absent, infer full-list ordering, upgrade global Source health, or change `StructureSnapshot.complete` to true. If the operation depends on information outside the bounded viewport, collect another Source read/scroll or refuse it.

## Implementation work packages and dependency order

| Order / owner | Files and responsibility | Depends on |
|---|---|---|
| 1. Contracts owner | `crates/interaction-contracts/src/desktop/source.rs`, `binding.rs`, `device.rs`, `samples.rs`, desktop schema/fixture exporters. Add mandatory object continuity, observer summary, binding continuity proof, local viewport scope and validation. | This decision. Freeze and export the protocol before parallel implementation. |
| 2. macOS Source owner | `native/macos/AXSession.swift`, `Accessibility.swift`, `AXTableProjection.swift`, `TableViewport.swift`, `Models.swift`. Separate global and per-object registration outcomes; mint per-read tokens; preserve global epoch meaning; compute closure and viewport proof without overstating notification guarantees. | Work package 1 contract shapes. |
| 3. Source adapter owner | `crates/source-macos/src/native.rs`, `native/watch/tracker.rs` and Source tests. Validate coverage counts/modes, keep observer health distinct from tree completeness, preserve global boundary monotonicity, and map bounded reason codes. | 1 and 2. |
| 4. Environment/Engine owner | `crates/interaction-engine/src/environment/binding.rs`, `state_core.rs`, `engine/evidence.rs`, decision/permit dependency handling and Engine tests. Selectively invalidate per closure; add the specialized viewport bind; keep global reset on real boundary changes; expire read-scoped decisions/permits on a newer read. | 1 and 3. |
| 5. Device/Guard owner | `crates/device-macos/src/backend/native.rs`, `guard/authority.rs`, `guard/submission.rs`, `native/device/AtomicSubmission.swift` and relevant AtomicClick/Scroll/table operation paths. Carry proof scope/source sequence through the request; atomically revalidate current target/context and submit only the admitted single action. Preserve stop/revoke/unknown semantics. | 1 and 4; requires accepted Source proof fields. |
| 6. Acceptance owner | Source, Engine, Device and end-to-end fixture matrices below; update milestone records only after actual native runs. | All preceding packages and frozen schemas. |

This decision does not modify `docs/design/complete-design.md` §26.7. The 20 fixture tasks × five seeds, 20 independent TextEdit tasks, language/theme/layout assignment, destructive overwrite confirmation, and existing completion denominator remain intact. Notification-covered simple controls alone are not a complete M2/MVP acceptance set; LIST-03 and LIST-04, modal layers, and all other frozen task conditions remain required.

## Required acceptance matrix

| Area | Required evidence before the gate can pass |
|---|---|
| Source identity registry | Covered nodes keep their ID only with valid registration and stable closure; every unsupported/read-scoped node gets a different ID on each read even when `CFEqual`; one-node failure does not increment the global boundary; global root/session uncertainty still resets it. |
| Tree and context | Fresh read-scoped IDs do not masquerade as topology changes; actual add/remove/reparent/reorder/label/group/window changes update tree/context evidence and invalidate every dependent proof. Unsupported semantic closure downgrades the binding. |
| Observer health | `available`, `degraded`, and `unavailable` are distinct and bounded. A partial gap is visible, does not make traversal completeness false by itself, and never silently leaves old target authority alive. Root observer loss remains fail-closed. |
| Binding and decisions | One read-scoped target’s next-read invalidation revokes its exact binding, pending decision and associated permit; an unrelated covered target can remain bound. Rebinding emits a new reference. A global boundary change still closes all target authority. |
| SortButton | Exact one-table/one-column current target succeeds once through the fresh atomic path. Duplicate, stale sequence, missing range, changed table, offscreen target, changed layer, failed current read or revoked permit yields no input. Unknown outcome is not retried. |
| LIST-03 viewport | The global Structure remains incomplete when unrelated traversal is incomplete; valid local viewport evidence supports only an in-scope action. Incomplete visible rows/columns, missing selection/settable evidence or out-of-scope targets are refused. |
| Frozen MVP | Run all §26.7 fixture and TextEdit variants with the frozen denominator and manifests. Record unsupported/failure/unknown outcomes; do not relabel a reduced persistent-only subset as full MVP. |

Required regression tests include Swift registry/observer and topology tests; Source adapter schema/health/boundary tests; Engine selective invalidation, stale-decision/permit and local-viewport tests; Device current-read uniqueness, context mismatch, atomic one-shot, revoke/stop race and unknown-no-replay tests; and the full macOS fixture matrix. These are acceptance requirements, not claims that the tests or live matrix already pass.

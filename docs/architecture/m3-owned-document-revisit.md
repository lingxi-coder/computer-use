# Owned document close and reopen

Main now exposes `registration.retainForRevisit(expectedText)` and an opaque
single-use `ticket.reopen()`. These establish file continuity and a new process
registration; they do not prove a controlled Save or satisfy Root UiRevisit.

Retention retires the old registration before another source bootstrap can use
it. The existing private launcher first confirms that its exact owned target
has terminated or that the original process identity has gone. Main preserves
the helper, then verifies the original private directory, its single document,
secure modes/owner/nlink, bounded no-follow FD reads, stable metadata and the
exact expected text digest. An atomic replacement can be captured at this
closed-process boundary. It is not a general registration renewal.

The ticket has no serializable fields and is consumed before reopen. Reopen
requires the captured file identity and bytes to remain unchanged, sends a new
run ID through the same private helper, and validates a new launch receipt with
a distinct process identity. A successful reopen produces a fresh immutable
registration; the old registration remains invalid. The operation does not
inherit an input grant. Lifecycle operations serialize with `stopAll`, which
can quit the retained helper and clean a pending ticket's file. Known
`not_launched` preserves cleanup ownership; uncertain outcomes cannot replay.

The launcher can see stale NSWorkspace entries for a just-exited process.
Launch now excludes such a cached entry only after an absent kernel identity,
`kill(pid, 0)` returning ESRCH, and a second absent identity. Live/reused PIDs,
permission failures and other unknown states still refuse launch. The kernel
inventory independently includes live TextEdit processes. Six pure cases and
the real lifecycle cover this boundary.

File hashing in both reopen preflight and receipt validation is bounded and
checks stable descriptor/path identity. Main's 31 focused lifecycle/launch
tests and 142 full tests pass. The final
[744e6618 lifecycle sample](../../artifacts/m3-owned-document-revisit-744e6618-99a8-4816-9303-5529e7da0812.json)
verifies the same private file, new process/registration, three current exact AX
value readbacks before and after reopen, retired old token, consumed ticket,
zero inputs/SDK calls and complete cleanup. The earlier failed samples remain
available and are not counted as task success.

The read-only
[89a0bb18 Save inventory](../../artifacts/m3-owned-focus-messaging-89a0bb18-e1e5-45f5-85af-543fe579415c.json)
finds one Cmd-S item in the complete immediate File-menu list, with enabled and
AXPress facts and a stable identifier digest. A deeper AXMenu returns
cannotComplete; the overall menu tree is therefore still incomplete. The probe
never executes an action. These facts may guide a scoped command profile, but
they are not Save authority or a completed save effect.

The lifecycle checkpoint above preceded the dedicated Save candidate/intent,
separate authority, Native commit/journal and retained-Root replacement. Those
are now covered by the [controlled Save/revisit workflow](m3-owned-root-revisit.md)
and the [temporary-document product entry](m3-owned-document-product.md).
Root verification consumes trusted file lineage and later fresh UI evidence.
`same_instance` remains strict; this isolated zero-input lifecycle does not
substitute for controlled Save/Close/Reopen, model quality or business-suite
acceptance.

Run this explicit lifecycle diagnostic with
`CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 node --import tsx scripts/smoke-macos-owned-document-revisit.mts --revisit`.

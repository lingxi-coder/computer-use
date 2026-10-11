# Ordinary row Root and fresh revisit

The registered row workflow keeps the confirmed public selector unchanged:
`ui.select_row → ui.revisit → goal.required@1`. Revisit has the Navigation
effect class. Its away/back actions use a left click with no modifiers only
on current AppPack page-navigation controls classified as Navigation. Other
activation remains on its existing route; row selection remains a dedicated
single `AXSelected=true` call.

A Revisit Unknown receipt ends the skill with EffectUnknown. A later page
observation cannot authorize its next navigation or turn that receipt into
Completed. Known input permits read-only verification against Source whose
observation time is strictly later than the input boundary. In particular, an
old snapshot at the setter's completion time cannot trigger a second toggle
that undoes a successful setter.

ExecutionCore records a synchronous, single, non-held Applied submission as
Completed immediately, including its return boundary. Multi-step, held-input
and asynchronous submissions keep their existing lifecycle. A later Source or
page invalidation cannot retroactively cancel the completed atomic submission.
For a Completed receipt missing `input_end_ms`, Engine uses the receipt's fixed
acknowledgement time as an observation floor. It does not use the command's
future deadline as a return time. Exact computer-batch prefix verification
continues to require explicit input timing; missing timing remains Unknown
and cannot cause replay.

Failure tests now distinguish completed input from an unacknowledged submission.
A default-off fixture fault applies the original input and returns Pending
without a completion reply. Source/foreground loss and recovery cases use that
fault to preserve Unknown, original-receipt queries and no-replay assertions.
The known-Completed read-only positive case stays separate. This fixture models
a missing acknowledgement; it does not validate native asynchronous behaviour.
Old-frame tests explicitly capture before the input instead of relying on a
future timestamp or on a delayed synthetic completion.

## Native diagnostics

The private, bounded diagnostic writer correlates each accepted reply with the
trusted pending Native Guard request, intent, attempt, Root, task revision and
session. Row and AXPress traces preserve the actual call count, signed AXError
and uptime timestamps. Click receipts establish native request completion;
they do not establish the number of OS events posted. The shared writer limit
remains eight records / 16 KiB, with a 4 KiB record limit and private permissions.
No additional AX reads or diagnostic I/O run inside the final Stop submit gate.

The [first diagnostic](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/native-row-live-1-analysis.json)
recorded one successful row setter, followed by one AXPress returning raw
AXError -25204 after 26.282541 ms. Although the fixture reached Overview, Root
did not complete and no back action was submitted. This is a recorded failure;
the elapsed time alone does not establish the AppKit cause.

The [second diagnostic](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/native-row-live-2-analysis.json)
completed the ordinary product authorization, Goal confirmation and Execute
path. It recorded one successful row setter and two applied native navigation
click requests. The fixture selection count increased once and revisit count
increased once. New complete Source uniquely matched the original
Weekly report / Mina / Review row in the frozen table scope, with selected=true;
that evidence belongs to the completed `goal.required@1 / ui_revisit` result.
Root causality remains Unknown: degraded observer coverage does not prove
continuous identity of the AX entity across navigation.

Both diagnostics used explicit reference planning, made zero provider calls,
and confirmed termination of their owned fixture instances. They are individual
diagnostic samples with `business_gate_eligible=false`. They do not satisfy the
canonical §26/§28 task denominators, model quality, latency percentiles,
physical Stop/fault matrix, recovery, resource, signed packaging or second
machine acceptance gates. The first MVP remains incomplete.

`llm-client` Decisions API implementation belongs to the other user session.
This project retains its existing fixed SDK revision until that implementation
has a completed contract and revision suitable for integration. Observed WIP
SDK files are not evidence of a released native provider endpoint or a live
call. The [session boundary record](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/decisions-api-external-wip-observation.json)
records this distinction.

## Final local checks

The [checkpoint](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/row-revisit-checkpoint.json)
pins the final source, binaries, inherited Electron output and all diagnostic
records. The final single-thread workspace run passed 1167 Rust tests across
58 suites with no failures or ignored tests. Desktop passed 260 tests; Native
Device passed 1142 pure checks. Strict Clippy, format, diff, dependency-source
identity and Rust binary build checks passed. Driver saved-data validation
passed 15 row/AXPress and 10 click cases. Earlier failures remain in the same
artifact directory. These checks do not establish concurrent stress, native
fault behaviour, provider calls or business acceptance.

The [archived-source proof](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/native-row-live-archive-and-revisit-proof.json)
records an actual away page and a returned Source observation strictly after
the last navigation return. It does not compare Source sequences across watch
sessions. The [SQLite proof](../../artifacts/m3-row-revisit-af15fe19-77e1-4b98-b5b8-a938ca7f692e/native-row-live-2-persisted-ledger-proof.json)
verifies the unchanged FrozenGoal and latest effect directly from the archived
database. Neither the database nor click receipt invents an OS event count.

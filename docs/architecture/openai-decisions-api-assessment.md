# OpenAI Decisions API assessment

Verified on 2026-10-05. OpenAI announced **Decisions API** at DevDay on
2026-09-29: Luna chooses among predefined finite answers using text or image
context. The announcement describes classification, routing and next-action
selection, and labels availability limited preview with wider release planned.
[Official announcement](https://openai.com/index/devday-2026-recap/).

The earlier interpretation of the user's API reference as Responses computer
use was incorrect. Decisions API and `computer_call.actions` are separate
capabilities. The existing Responses integration remains implemented; it does
not establish Decisions API access.

The 2026-10-05 exact official documentation search for `"Decisions API"`
returned no matches, and the public OpenAPI endpoint inventory did not list a
Decisions endpoint. These are discovery results, not proof that private preview
access is unavailable. The public documentation searches did not yield an authoritative
Decisions HTTP endpoint, request/response schema, cancellation or usage contract.
The pinned local SDK/runtime also has no Decisions adapter. Account access,
current rollout, pricing, latency and calibration remain unverified. Do not
invent `/v1/decisions`, assume a Responses-compatible payload, or enable a
production route from the announcement alone.

The current discovery record is
[saved with the local validation artifacts](../../artifacts/m4-capture-sample-timing-69cbfc02-6f07-4f11-8e4a-68eea8f0764b/decisions-api-discovery.json).
It records the exact zero-hit documentation query and 227-entry public endpoint
inventory, without inferring private-preview availability from either result.
The latest user clarification was checked again on the same date against the
official announcement, the documentation search and the public endpoint list.
The discovery result is unchanged; the separate
[recheck record](../../artifacts/m2-sort-column-e8a1571d-7035-4548-8e8e-4e1ff8356ed6/decisions-api-recheck.json)
retains that evidence. It is not a successful provider call or an account-access
test.

An additional [current recheck](../../artifacts/m2-row-viewport-8e59e648-364b-4830-80bc-dde9eb9c790f/decisions-api-current-recheck.json)
again fetched the official announcement and queried the exact official docs
topic and endpoint inventory. It still does not establish an authoritative wire
contract or test this account's preview access.

The [row-goal integration recheck](../../artifacts/m3-row-goal-2220c0e5-1f37-4e6d-9e04-d02afb00f88d/decisions-api-wire-recheck.json)
preserves the latest exact documentation query and public endpoint inventory.
The announcement remains available; the query again returned zero hits and
none of the 227 listed paths names a Decisions endpoint. No account-access or
provider-call test was performed.

The project's `DecisionRequest.questions` already carries explicit question IDs
and option keys, while its bounded state contains candidate IDs and a candidate
set digest. This makes a separate Decisions backend a plausible fit for closed
candidate selection. That is an architectural inference, not proof of API
schema compatibility. Preserve the request generation, original Root/task/epoch,
plan and snapshot revisions, deadline and candidate digest through the local
adapter. Reject missing/foreign/duplicate answers and selections outside the
original option set. A returned score is not a calibrated probability.

The first application integration point is the S1 teacher decision lane:
`TeacherCallTransport` in `crates/harness-adapter/src/teacher.rs` and Host's
`start_teacher_decision`. It is separate from S2's
`PlanningRequest → PlannerOutput`. Build the offered answer-key-to-`PolicyChoice`
mapping locally; initially allow executable offered candidates and fixed
ObserveMore/RequestPlanning/StopOrYield choices. `ContinueSkill` is currently
rejected by Engine. Preserve source `Teacher` and the existing decision-call
accounting. Unknown usage or confidence stays absent. The current teacher
adapter expects verified ToolCall/ToolResult events; a Decisions result needs
an explicit typed-result boundary rather than assumed matching wire events.

Once an authoritative wire contract and access are available, provider
communication belongs in `llm-client`, runtime invocation in Harness, and the
application's projection/result mapping in `harness-adapter`. The local Engine
still owns admission, current bindings and ordinary durable Attempt execution;
the model chooses a proposal and cannot grant permission or directly submit
input. Keep the Decisions route distinct from the existing ordered computer
batch route and explicit about its approved data purpose.

The user confirmed that Decisions API will be added in `llm-client`. Keep that
client work independent of the current local row-execution implementation; the
application must consume the client's published interface rather than duplicate
the provider protocol or infer it from the announcement.

The local answer boundary now validates exact question identity, complete and
unique answers, option membership, bounded selections and confidence range in
`DecisionRequest::validate_answers`. An empty Choice can report no selection;
it cannot accept an invented option. Generic business Choice options are still
valid and need not be candidate IDs. Score retains its existing bounded value
semantics, and Noul has no selected option; neither is silently promoted into
an action candidate.

Engine's current `DecisionBook::open` constructs one action Choice from its
executable candidates. It revalidates every backend's answer against that exact
question. A selected candidate and an explicit StartSkill must agree. Explicit
control choices must match the selected control candidate when a selection is
present. Candidate fallback is confined to this internally constructed action
question. Future batched classification or progress questions require a
registered program mapping before they can choose an action; their option text
is not such a mapping. Invalid proposals close the durable pending decision;
memory is cleared only after the close commits successfully.

A hosted Decisions backend can be evaluated as a teacher or alternate decision
backend. It does not itself prove the local CPU S1 latency, offline behavior or
the full quality gate in `complete-design.md`. Compare identical frozen cases
for coverage, correctness, refusal, total latency, available usage and
calibration before changing a runtime lane. Provider access does not establish
native Source/Device identity, Stop, input release or business verification.

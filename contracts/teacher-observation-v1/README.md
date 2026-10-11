# Private teacher observation v1

This protocol uses Host's separately inherited FD4. It is not a desktop IPC
Message, a model tool, a renderer endpoint, or an execution-result channel.
FD3 remains reserved for the owned Root revisit control protocol.

Each message is UTF-8 JSON preceded by a four-byte big-endian body length. The
complete frame, including that prefix, is at most 128 KiB. Unknown fields,
unsupported versions and invalid embedded decision contracts are rejected.
Counters must fit JavaScript's safe integer range.

The three messages are:

- `started`: `{version:1, type:"started", request:DecisionRequest}`.
- `finished`: `{version:1, type:"finished", scope, proposal:DecisionProposal}`.
- `cancelled`: `{version:1, type:"cancelled", scope, reason}`.

Scope contains exactly `decision_id`, `request_generation`, `root_task_id`,
`root_epoch`, `task_revision`, and `candidate_set_digest`. Terminal messages
refer to the request originally admitted by the teacher. Finished proposals
have source `teacher` and matching decision/generation/candidate digest. Reasons
are `cancelled`, `expired`, `quarantined`, `accounting_failed`, `scope_mismatch`,
and `shutdown`.

Host uses a separate queue with capacity two and a nonblocking background
writer. A frame has a 100 ms write deadline. Overflow drops observation work;
a broken or stalled observer retires only this channel. Main validates scope,
deadline and current candidates before comparing a local S1 answer. Missing or
late observations remain unverified, and none of these messages can submit an
input or alter the original teacher result.

`fixtures.json` contains synthetic Started, Finished and Cancelled objects
checked against Rust serialization and the TypeScript decoder. It contains no
provider credential or actual user task. Golden conformance does not prove live
provider, model quality, input release, or full MVP acceptance.

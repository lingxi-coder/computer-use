# Teacher shadow observation channel

The local Laya backend and utility-process lifecycle are implemented and have
[real CPU/Electron evidence](m0-laya-local-runtime.md). The Main
`ShadowDecisionRunner` primitive observes two runners while returning only the
primary delivery. Its counters carry no document text or decision identifiers.
It does not replace an unavailable primary with a shadow answer. The product
now connects authorized Rust teacher calls to a separate TeacherShadowObserver
through private FD4; the local model remains observational.

## Required isolation

Host handles authorized teacher calls internally in `start_teacher_decision`
and `poll_teacher`. The ordinary DecisionRequest relay is emitted only when
there is no Rust teacher. Reusing that relay for teacher shadow would route
observations through an execution-result channel. Waiting until the teacher
finishes before starting local inference would also consume the original
decision deadline.

Shadow messages must use an independent transport. Host's ordinary
`send_supervisor` marks the Host for exit on send failure, and the queued writer
permanently fails when its queue fills. Extra observation traffic on that
transport could interfere with the primary task. FD3 is already reserved for
the private owned Root revisit control protocol. FD4 is available for a new
Host-only, Main-owned observation channel.

The implementation is present:

- Main creates FD4 only for an authorized S1 teacher session. It reads a private
  versioned, length-prefixed stream with a fixed frame and buffer limit; it
  exposes no renderer endpoint.
- Host admits observations with `try_send` into a separate small queue. A
  background writer owns FD4, uses nonblocking bounded partial writes, and
  retires its channel on write failure. Queue overflow drops observation work.
  None of these failures alters the ordinary Link, Host exit state, or Engine.
- `Started` carries the bounded typed request after teacher startup succeeds,
  so local inference runs concurrently. `Finished` carries its explicit scope
  and typed teacher proposal after scope/accounting validation. Cancelled,
  expired, quarantined or failed calls retire their shadow state.
- Main correlates decision ID, generation, root ID/epoch, task revision and
  candidate-set digest. Missing, invalid, late or unavailable observations
  remain unverified. Semantic comparisons ignore confidence differences.
- Local proposals and comparison counters never return to Host or Engine.
  Original teacher admission, model-call accounting and ordinary cancellation
  remain the authority. A slow or failed observer cannot delay Stop.

The implementation is in `teacher_observation.rs`, Host startup/shell hooks,
Supervisor FD4 handling, and `teacherShadowObserver.ts`. Stop sends the Device
command before closing the observer. Model startup has an immediately closable
Main lifecycle handle. An explicit new Root epoch retires the old local fence.

[The frozen checkpoint](../../artifacts/m0-teacher-shadow-checkpoint-95112670-f4a6-46e0-a1ca-5b543961d180.json)
records Host 70 and Desktop 225 passing checks, strict types/schema, Host Clippy,
format and Electron build. Three Host cases use real Engine-generated requests
and an injected SDK transport to verify original teacher admission/accounting,
observer failure isolation, and a single Cancelled terminal after Stop.

[Actual Electron CPU observation](../../artifacts/m0-laya-utility-a1a2a2ef-4d52-4f57-99e5-38a79e8d4b75.json)
loads the pinned model in real child processes and consumes a clearly synthetic
teacher result. Local confidence refusal remains unverified; all three children
exit, with zero provider calls and physical inputs. The run has stable source
and build hashes. It is not a live OpenAI teacher or task-quality acceptance.
Shared-load performance, full business runs and signed installation remain
open, as does the full MVP.

## OpenAI interface review

The current [official computer-use guide](https://developers.openai.com/api/docs/guides/tools-computer-use)
supports Responses `computer` calls with an ordered `actions` array. The
application executes permitted actions and returns a `computer_call_output`
screenshot with the matching `call_id`, then continues with
`previous_response_id`. The guide also supports existing function-based UI
tools. A completed model call is not an execution or business receipt.

The existing teacher uses Responses with a forced `TeacherDecision` function
and accepts only the supplied decision candidates and question options. This
route remains usable for the current proposal contract. The pinned llm-client
revision `0c6a907d897a54e656700c00cf335d91b10dca0b` does not expose a typed
computer action/screenshot loop: unknown provider items are retained as native
content, while this teacher adapter consumes function tool-call events only.
Enabling the native computer tool therefore requires typed SDK support and a
bounded Harness loop through the existing permission, binding and execution
owners. It must not decode provider JSON directly into Device input.

This review establishes API compatibility and the remaining integration work;
it makes no live-provider claim. Product credentials, purpose grants, actual
teacher quality and S2 task execution remain unverified.

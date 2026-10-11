# OpenAI computer integration boundary

OpenAI's current public computer-use integration uses the
[Responses API `computer` tool](https://developers.openai.com/api/docs/guides/tools-computer-use).
It returns an ordered `computer_call.actions` array. The application executes
permitted actions and sends `computer_call_output` with the matching call ID and
an updated screenshot, continuing through `previous_response_id`. This is an
available decision interface for a locally owned browser or desktop. A generated
call's completed status does not prove local execution.

The current guide also describes code execution as its recommended integration
for GPT-6 Astra, while retaining structured `computer` actions and custom UI
tools. That recommendation does not require this application to run generated
code: its typed proposal boundary and local permission checks remain applicable.
OpenAI separately announced **Decisions API** at DevDay on 2026-09-29. It uses
Luna with text/image context and predefined finite answers. The announcement
labels it limited preview; this project's account access and exact endpoint,
schema and latency have not been verified. See the
[Decisions API assessment](openai-decisions-api-assessment.md). The earlier
identification of the user's Decisions API as this Responses computer tool was
incorrect. These are separate integration paths.

The official computer-use guide was refreshed on 2026-10-05. This document
records the implemented Responses path and its open acceptance gates. A typical
generated item has this shape:

```json
{
  "type": "computer_call",
  "call_id": "call_002",
  "actions": [
    { "type": "click", "button": "left", "x": 405, "y": 157 },
    { "type": "type", "text": "penguin" }
  ],
  "status": "completed"
}
```

Here `completed` describes generation of the call. It is not a Device receipt,
an effect result, or permission to execute the batch. Our adapter must preserve
action order and correlate the actual observation output with `call_id`;
unsupported operations cannot silently become a keyboard or pointer fallback.
The official guide also supports an initial screenshot-only call, which must
not emit input.

The [September 29 release entry](https://developers.openai.com/api/docs/changelog)
and the 2026-10-04 official documentation refresh also confirm the
[Agents API computer-use tool](https://developers.openai.com/api/docs/guides/agents-api/tools/computer-use).
That route runs a browser in an OpenAI-hosted environment using agent sessions,
events and browser-origin approvals. It does not directly execute our local
macOS TextEdit session. For this product's local desktop, the Responses
`computer` tool or our existing custom UI tools remain the relevant interfaces.
This integration choice follows the documented ownership of the execution
environment.

The existing product S1 teacher route uses Responses with the forced custom
`TeacherDecision` function. It accepts only the expected tool-call name and
validated TeacherOutput JSON. This remains a supported integration style:
OpenAI documents keeping
[custom UI tools](https://developers.openai.com/api/docs/guides/tools-computer-use-integration#use-your-own-ui-tools).
Provider selection alone does not provide a key or restore either outbound
purpose grant.

The previous product llm-client revision was
`0c6a907d897a54e656700c00cf335d91b10dca0b`. Its Responses codec could carry
raw provider items but does not expose typed computer calls, actions or outputs.
`ToolSpec.tool_type` and extra fields can serialize a raw computer tool;
`ToolChoice::Tool` still selects a function. The response decoder and stream
decoder preserve non-function items as ProviderContent. ToolResult emits
function_call_output only. Scoped ContinuationRef already carries a response ID.
Those escape hatches did not provide a validated computer-action adapter.

Any typed provider-wire extension belongs in llm-client's protocol tool/message
types and Responses encoder, decoder, stream decoder and codec tests. Harness
may then expose a distinct typed computer-event path; the current S1 Recorder
ignores ProviderContent. Application code must not add a parallel HTTP client or
parse opaque provider JSON into executable device commands.

The local decision compiler must map permitted model proposals into current
typed candidates and the ordinary durable Attempt workflow. Environment owns
fresh bindings; TaskRuntime owns goals, budgets and cancellation; Execution owns
permissions and commit; Verification owns effects. Coordinates, keys and text
returned by a provider cannot grant permission, supply a trusted binding, bypass
the Device gate or prove a business effect. Unsupported actions, stale views,
unknown receipts and unmatched call IDs must remain non-executable.

Product computer integration and live-provider acceptance are still open. Actual
S1/S2 calls additionally require the product's configured key and separate
purpose grants. The successful local owned-text diagnostic made zero SDK calls.

The [SDK source checkpoint](../../artifacts/m4-openai-computer-00c081fc-4bf1-440f-af7a-7d2726341098/sdk-checkpoint.json)
on 2026-10-05 implements the current structured computer protocol in the canonical
SDK working tree based on `1a60e73ad12e663be768232a91cbd3ea693aca6e`. The changes
are uncommitted and exported as an exact baseline patch. Typed provider-owned
call/output formats use `ContentBlock::Native` and `StreamEvent::Native`; the
computer-tool marker is a request native option, separate from provider-executed
hosted tools. The Responses codec preserves action order, validates the complete
terminal call set before publishing it, rejects duplicate call IDs and inconsistent
terminal status, and emits a distinct screenshot output item. Other protocols
reject the marker and typed contents during request validation. Raw computer
provider items cannot bypass that typed path.

The checkpoint records 272 default SDK checks, including 26 new computer checks,
and eight WebSocket loopback tests. Library and new-test strict Clippy and format
checks pass. All-targets Clippy retains four pre-existing transport-test lint
failures. All failed checks and source pins are preserved. These are SDK protocol
and mock-transport results, with zero live provider calls and zero desktop input.

Before changing product Cargo pins, the subsequent
[isolated Harness checkpoint](../../artifacts/m4-openai-computer-00c081fc-4bf1-440f-af7a-7d2726341098/harness-checkpoint.json)
migrates the exact `4dfd9abef1dc293a9617d35387165cb2256b5d98` baseline to SDK 0.3.
OAuth, PKCE and STS protocol helpers now follow the SDK's existing host-owned
authentication boundary. Pricing uses SDK quotes and preserves conservative
bounds, unknown-price refusal and fixed standard overrides. The raw ModelRuntime
stream preserves typed computer calls, order and terminal usage; malformed later
actions and duplicate call IDs emit no Native prefix. The legacy history projection
explicitly refuses those typed items. The isolated scope passes 1070 tests,
seven final bounds checks, strict scoped Clippy and formatting. Both dependency
patches apply to their exact baselines. Earlier failures remain preserved.

That earlier validation used a temporary seven-crate assembly and a local SDK path overlay.
The task has not edited the canonical dirty Harness checkout; a
[scope audit](../../artifacts/m4-openai-computer-00c081fc-4bf1-440f-af7a-7d2726341098/canonical-harness-scope-audit.json)
records independent shared-working-tree status drift without claiming global
immutability. These checkpoints do not establish product integration or release
source identity. The product still needs a fresh-frame bridge, a pending call/output
pairing ledger and a candidate compiler before any computer proposal can reach
ordinary Attempt admission. A scoped continuation alone does not identify the
pending call ID; the caller must retain the original call and require the SDK's
`validate_for_call` pairing check. Public DTOs never grant input permission.

The 2026-10-05 product assembly now pins independent local commits:
SDK `0a6dcb5a2135de4d8b041db4896f8889d9d0f4c0` and Harness
`0abe9ea0fb1e1866db434a29aef35ab0b22234ee`. The Harness commit fixes its SDK
dependency and submodule to the same SDK commit. Product manifest and lockfile
retain canonical Git URLs and complete SHAs. Locked offline Cargo metadata and
the pinned runtime compile pass using command-scoped local Git URL rewrites and
an isolated Cargo Git cache; no product path overlay or global Git configuration
was added. These commits are unpublished. A normal remote fetch, CI build and
release-source verification remain open.

Route B enforces this local source identity through
[`scripts/check-model-source.sh`](../../scripts/check-model-source.sh). By default
it reads Cargo metadata with `--locked --offline --all-features`. The optional
`--metadata FILE` argument validates captured JSON without invoking Cargo. The
gate checks the canonical Git URLs and full manifest SHAs, exact resolved source
IDs, unique Harness and SDK package identities, the shared Harness checkout
layout, and reachability from product workspace members through `llm-runtime` to
the SDK and required Harness crates. It rejects mixed pins and reachable path
overlays. This local resolution check does not establish that either commit has
been published to its remote.

Provider-neutral fences and ordered proposals are published separately in
`contracts/computer-v1/schema.json`. Their generated TypeScript view comes from
the Rust contract, without changing the desktop control envelope. A separate
bounded binary packet correlates a COMPLETE image with the requested fence and
exact FrameMeta. Source refresh acknowledgement alone cannot supply that image:
the watch restart is asynchronous, and the actual fresh COMPLETE record must
arrive before continuation. IDLE callbacks retain the original pixel timestamp.

The [product component checkpoint](../../artifacts/m4-product-computer-7e8313b5-653e-4695-be8c-2285f2d47e03/product-checkpoint.json)
records 993 passing workspace Rust tests, including ten injected-transport
computer roundtrip regressions, plus 32 TypeScript protocol/image checks and
eight source-gate parser checks. Workspace strict Clippy, format, generated
types and TypeScript checks pass. Earlier failed runs are preserved.
The independent computer transport uses the SDK's native call/output types and
scoped continuation. It requires a new screenshot after the original screenshot
request, and after a complete action outcome uses that outcome's time as a
capture barrier. A newer frame sequence captured before that barrier is refused.
PNG validation includes container and dimensions; missing stream provenance
returns a closed error. Explicit pending safety checks must match completely.
Model text and normal end-of-turn are observations, not business completion.

At that earlier component checkpoint, the ordered batch was in memory and
emitted inert step proposals; Host image FD integration, target compilation
and ordinary Attempt admission were still open. The later Host/Engine wiring
below supersedes that status. The checkpoint made zero live provider calls and
zero desktop inputs, and did not satisfy the full MVP gates in design sections
26 and 28.


## Frozen observation checkpoint (2026-10-05, earlier stage)

The independent `computer_proposals` purpose uses a Main-generated account
scope after explicit model-purpose approval. Main retains that approval while
the replacement session observes without a window grant. It sends the computer
purpose, scope and private key to Host only after a fresh window authorization.
The resulting FD5 carries bounded CUCIREQ1 requests and CUCIMG01 images or
CUCIREJ1 rejections, with one outstanding request. Source remains Main's sole
image producer. Stop and source/peer boundaries permanently close the pixel
port and revoke this computer session; reconnecting requires fresh consent.

The polling computer worker runs provider I/O, PNG validation and encoding away
from the Host control thread. It returns a request-local closing result after
the stream has dropped. Late cancellation cannot publish actions. Available
usage survives malformed output; missing usage remains unknown. Pixel I/O uses
a deadline that shrinks between reads and is interrupted by shutdown. Callback
cancellation is propagated even when its error kind is Interrupted; it is not
retried as an operating-system interruption.

Environment owns the opaque frozen observation and its original AX tree. The
current geometry proof accepts complete standard windows whose unpadded content
rectangle, image scale and AX window geometry match exactly. Cropping, differing
scales, sheets and menus are rejected. Reaction preflights the whole batch,
keeps the original deadline, maps coordinates to one authorized original
instance, and rebinds that same instance against fresh state. Left clicks are
accepted only when ordinary ui.activate emits the equivalent pointer center;
AXPress substitutions, modifiers, and unsupported action kinds are rejected.
Wait and screenshot remain observation steps.

At this checkpoint, the computer lane still lacked a production Begin trigger,
model-call accounting and a normal Attempt consumer. The integration progress
below supersedes that checkpoint. This earlier observation path does not claim
source SHA publication, live OpenAI acceptance or physical-device/business
coverage.

## Host and Engine integration progress (2026-10-05)

The current code connects Engine `Begin` commands to Host `ComputerSession` for
an eligible, explicitly enabled `ui.activate` subgoal. Each provider call is
opened in the durable `computer_model_calls` ledger before network dispatch and
closed against its full call identity; unknown usage stays unknown, and
shutdown/cancellation can close a late completion without installing its
proposal. Provider continuation state is scoped to that approved subgoal and
reset when the scope ends, so a later subgoal or Root cannot inherit its
response chain. The worker uses the SDK's typed Responses computer call/output
path.
After a batch passes whole-array validation, Engine compiles supported steps
through the ordinary DecisionBook and durable Attempt path, then requires a
fresh observation and rebinds the same semantic instance before another input.

Host and Engine exchange screenshots and safety requests over private FD5.
Computer screenshots require an exact complete-image/AX pairing:
`StructureSnapshot.frame_ref` must equal
`FrameMeta.stream.image_frame_id`, the PNG content ID. The public
`FrameMeta.frame_id` event ID and a matching sequence number are not substitutes.
The same read must prove the complete active window or supported sheet and its
geometry. The computer path waits for this stream evidence and does not issue
`SourceCommand::RefreshNow`, which restarts the watch and invalidates its
current authority. A same-watch `CaptureNow` control request is still only a
design in [the fresh-capture plan](m4-computer-fresh-capture.md); a static
display that produces no newer complete stream frame remains an open capture
case.

Nonempty provider safety checks go through the FD5 safety request/reply path.
The Host requires the exact checks to be explicitly confirmed. A declined or
unavailable confirmation closes the pending computer scope and pauses the
Root, so the same subgoal cannot fall through to ordinary planning. Computer
actions remain a proposal until Engine admission succeeds.

The wire contract represents nine action kinds, but the current Engine
capability set is limited to `click`, `wait` and `screenshot`. The current input
compiler accepts only a proven equivalent unmodified left-click at the center
of one authorized semantic target; targets with an `AXPress` substitution are
rejected. Double-click, drag, move, scroll, keypress and type remain unsupported
for this path. A durable model-call ledger and scope reset do not make all nine
operations native or complete the product gate. Source `ObjectRef` to Device AX
entity identity, same-watch `CaptureNow`, remaining native actions, real
provider/desktop acceptance and the full §26/§28 matrices remain open. The
current [local checkpoint](../../artifacts/m4-computer-attempt-2670da99-4821-424e-8e76-ba5730ff5e01/product-checkpoint.json)
records 1056 workspace Rust tests, 437 separately checked Engine tests, 66 scoped
TypeScript tests and eight source-parser tests. Strict workspace/all-target
Clippy, formatting, generated types, typecheck, Rust binary and Electron builds
pass. Failed attempts remain recorded. Live provider calls and physical desktop
inputs were zero; this does not establish native or full MVP acceptance.


## Same-watch capture implementation (2026-10-05)

The current `FrameMeta.provenance` union explicitly separates stream pixels from
single capture operation bounds. Host, Source, Environment, image IPC and Main
handle both branches. Capture acceptance retains correlation for later failure;
it is never a refresh or freshness receipt. Same-watch content replacement keeps
real SCK heartbeat separate and retains requested PNG bytes beside newer stream
images. Source replacement and cancellation cannot publish a foreign result.

See [fresh-capture implementation and remaining gates](m4-computer-fresh-capture.md).
Local checks pass, but the readonly static-window attempt did not obtain initial
COMPLETE pixels and did not reach CaptureNow acceptance. Native same-watch,
WindowServer incarnation, timing, all action semantics and full MVP remain open.
This is independent of the announced Decisions API, whose public wire contract
and account access have not been established.

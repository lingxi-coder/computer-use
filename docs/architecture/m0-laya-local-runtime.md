# Local Laya CPU runtime

The local S1 runtime uses the exact multilingual model revision
`e4e9ddf21a7b1903b7acffd8814ad4307bf63a67`, the checked package manifest, and
`onnxruntime-node` 1.30.0. Its six model/graph/tokenizer resources are verified by
size and SHA-256 before session creation. The backend executes only the CPU
encoder and decision head, accepts one bounded choice question over current
executable candidates, and returns an S1 proposal. It does not execute input.

Pinned upstream preprocessing is kept unchanged as TypeScript source. Generated
JavaScript and declaration files form a separate compiler boundary, with source
and output hashes checked by tests. The application's strict TypeScript settings
remain enabled. Model weights are external resources; they are not committed to
the decision-worker package.

Electron builds `localDecisionWorker.js` alongside Main. Main owns its absolute
build path and the private model manifest path. The utility transport carries
one versioned, length-prefixed JSON message, bounded to 128 KiB, and validates
the existing DecisionRequest/DecisionProposal schemas. The lifecycle has one
active request and a bounded queue. Cancellation, generation, root revision,
epoch, deadline and process death fence results. An exited worker is terminal
for that instance; it does not automatically restart or replay requests.

Backend cancellation prevents a late result from being delivered. Native ONNX
work can still be draining, so sessions are released after the active operation
finishes. Main provides the bounded process-termination fallback. Missing or
invalid resources produce an unavailable S1 result; there is no Python or
scripted-teacher fallback for a native session.

## Evidence and limits

[Actual Node CPU numerical parity](../../artifacts/m0-laya-node-cpu-dd118794-7a39-424a-bc37-73ac73173359.json)
covers six cases in three mixed batches. Probabilities match the existing Python
reference within `4.60e-8`; two disagreements with the human expected answer are
retained. The three inference samples are not a p95 or shared-load report.

[Actual Electron utility lifecycle](../../artifacts/m0-laya-utility-3d78f194-1c2e-4b66-9c4e-62435bf9dcab.json)
loads the fixed model in two distinct child processes, returns a correlated S1
proposal, drops cancelled and invalidated work, observes child exit after close,
and drops pending work after forced child death. Cancellation delivery in this
run takes 0.324 ms; process close takes 393 ms. Neither measurement establishes
the shared-load input-release or stop gate. It makes zero provider calls
and physical inputs. That artifact records its exact source/build hashes; later
changes require a new immutable run.

The raw confidence thresholds are plumbing defaults. These checks do not prove
domain fine-tuning, calibration, candidate permutation stability, task quality,
joint-load latency, packaging, or signed installation. The model is not an
authoritative product decision backend. M0 and the full MVP remain open.

After building the desktop, repeat the local lifecycle check with an existing
Electron executable:

```sh
node --import tsx scripts/smoke-laya-utility.mts --runtime /absolute/path/to/Electron
```

The check creates a fresh immutable artifact and temporary test files, and does
not start Host, Device, an external application task, or a provider connection.

The [frozen foundation checkpoint](../../artifacts/m0-local-s1-checkpoint-3065716a-64ae-4574-93b1-ed592726a7eb.json) records Desktop 209 and decision-worker 19 checks,
strict types/schema/build checks, and preprocessing build reproduction. The
Main shadow primitive has 12 checks. The subsequent
[independent private channel](m0-teacher-shadow-channel.md) is implemented and
covered by Host/SDK-injection and Electron CPU checks; live provider and task
quality remain unverified.

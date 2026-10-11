# M0 Laya CPU latency scan

This scan measures the pinned Laya backend on the local CPU. Run it from the repository root with:

```sh
node --import tsx scripts/benchmark-laya-cpu.mts [--threads 1|2|4]
```

It writes a new `artifacts/m0-laya-cpu-latency-<uuid>.json` file using exclusive creation. It never overwrites an earlier artifact. Run failures also leave an artifact with a fixed failure code and whatever bounded samples completed.

With no argument, the scan uses two intra-op threads. `--threads` accepts exactly `1`, `2`, or `4`; duplicate, malformed, or unsupported values fail before the scan starts. The parent passes the selected value explicitly to each profile worker, which checks that the real runtime receives the same setting.

The scan calls the real `LayaDecisionBackend.create` with the pinned revision `e4e9ddf21a7b1903b7acffd8814ad4307bf63a67`, installed `onnxruntime-node` 1.30.0, CPU execution, the selected intra-op thread count, one inter-op thread, and the production confidence and action-probability thresholds. A wrapper around the real runtime counts encoder and head calls and times session creation and each graph call; it does not replace either graph. Before measuring, the script verifies the pinned manifest and tokenizer, loads the pinned tokenizer, and runs the same `buildSequence` preprocessing as the backend. The artifact records the thread setting and fixed refusal thresholds as part of its configuration hash.

Protocol revision 2 fixes all three synthetic profiles to one choice question
with two options, at token ranges 250–350, 370–450, and 460–510. The latter two
use deterministic filler selected by tokenization before any ORT session is
created. The complete typed candidate state requires more tokens than the first
protocol anticipated: the two-option base uses about 301 tokens, and the 8/16
option requests truncate at the unchanged 512-token limit. Their capacity
preflight is recorded separately with zero inference and zero latency samples.
The earlier failed artifacts
[`5055d040`](../../artifacts/m0-laya-cpu-latency-5055d040-6366-4f05-9154-93429ab0293e.json)
and [`c6419a11`](../../artifacts/m0-laya-cpu-latency-c6419a11-27a7-418f-8349-dec1a8a36c93.json)
remain available; neither reached model inference. The target bands, selected
filler count, request hashes and configuration hash are recorded before timing.
Inference results never alter a profile. All accepted profiles retain option
markers, avoid per-option head truncation and stay within the original limits.

Each accepted profile attempts five warmups followed by 100 hot samples. Every sample records its sample/resource indexes, synthetic request hash and state size, token and marker details, wall and backend latency, encoder/head call counts and timings, failure or refusal, and process RSS. Refusals remain in the sample set. A sample contributes to latency percentiles only when exactly one encoder call and one head call completed. A profile is marked complete only when all 105 samples ran both graphs. Pre-inference refusals, timeouts, graph errors, and process-deadline termination remain visible and cannot count as model latency samples.

The artifact separates backend creation time, encoder/head session-load time, their estimated verification/setup residual, and the first decision call. It includes before/after hashes for the benchmark/backend preprocessing sources, model package manifest, and installed platform runtime native files. It also records OS, CPU model, logical CPU count, memory, and start load average. Power status is left explicitly unsampled for the parent run to reference if available. The coordinator imposes a 180-second limit per profile and a 600-second total limit, with a reserve for final hashes and artifact writing.

This is an offline synthetic inference timing scan only. It makes no claim about model quality, task or business correctness, stop/observe behavior, the §26.8 static gate, joint-load latency, a controlled action trace, physical device behavior, or MVP completion. It starts no Host, Device, or provider and sends no physical input.

The first complete revision-2 scan is
[`5a42ee43`](../../artifacts/m0-laya-cpu-latency-5a42ee43-ea71-46c4-9419-aba8180737cc.json).
All 300 hot samples ran both graphs; source/runtime hashes stayed stable and all
sessions released. Actual sequence lengths were 264, 401 and 475 tokens. Their
p95 wall latencies were 417.27, 668.76 and 815.72 ms; the latter two exceed the
design's 500 ms target. All synthetic decisions were refused for confidence,
which is retained in the report and establishes no quality result. Each worker
peaked near 1.42 GB RSS. These measurements are separate from formal static and
joint-load action traces and do not pass those gates.

## Frozen intra-op thread comparison

Run the one-time comparison from the repository root with:

```sh
node --import tsx scripts/compare-laya-cpu-threads.mts
```

Before inference starts, the runner writes an immutable UUID plan artifact. It verifies the
baseline report `5a42ee43` by SHA-256, checks the baseline host identity, and freezes the three
profile request-template and sequence hashes. A mismatch leaves the plan and a failed or
not-started comparison artifact; it does not substitute new inputs or a different host.

The runner then invokes the benchmark once per arm in the strict order 1, 2, 4. Arms run
sequentially in isolated profile workers. Each arm uses the same model revision, ONNX Runtime
1.30.0 CPU provider, inter-op count 1, confidence floor 800/1000, action-probability floor
500/1000, three synthetic profiles, five warmups, and 100 hot samples per profile. Only intra-op
threads vary. Profile and arm deadlines remain 180 seconds and 600 seconds; the comparison has
a 1,860 second overall deadline and allows 1.5 seconds of bounded child cleanup. The runner
does not repeat a failed, partial, or slower arm and never runs arms in parallel.

The comparison artifact references every arm's raw report by relative path and SHA-256, including
partial reports. It also records profile p50/p95/max, refusal counts, graph-call coverage, model,
runtime, source, and input-identity checks. Per-sample request hashes remain in the raw reports;
cross-arm content equality is frozen by the request-template and sequence hashes, so request
identity metadata is not mistaken for model-input changes.

An arm qualifies only when all three complete profile p95 values are at or below 500 ms and its
model, runtime, host, source, and input identities match the plan. If more than one arm qualifies,
the runner selects the lowest worst-profile p95; an exact tie prefers two threads. If no arm
qualifies, it makes no offline latency recommendation. The product runtime stays at two threads
in every case; the comparison never edits product configuration. Refusals remain included in
latency, and this synthetic scan does not measure semantic quality, business correctness, joint
load, or formal MVP acceptance.

The frozen sweep completed in 693.14 seconds on the same reference host.
[`2be3e518 comparison`](../../artifacts/m0-laya-cpu-thread-sweep-comparison-2be3e518-84fd-4945-839c-95c90384fca1.json)
references all three immutable arm reports and their SHA-256 hashes. Each arm
completed 300 hot samples with both real graphs; all identity checks passed.

| Intra-op threads | 264 tokens p95 | 401 tokens p95 | 475 tokens p95 |
| --- | ---: | ---: | ---: |
| 1 | 789.92 ms | 1249.45 ms | 1531.45 ms |
| 2 | 413.51 ms | 666.99 ms | 820.23 ms |
| 4 | 281.64 ms | 400.36 ms | 463.15 ms |

Four threads satisfy this offline scan's fixed latency rule. All synthetic
decisions were still refused at the unchanged confidence threshold. Product
configuration remains two threads; joint-load cancellation and quality
acceptance remain open before applying the offline recommendation.

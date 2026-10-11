# MVP business-result aggregation

`business-results.mjs` uses the fixture IDs and denominators from design §26.7:
20 fixture tasks × seeds 1–5, and 20 independent TextEdit tasks. Its
`TEXTEDIT-01..20` IDs are report slots defined here, not task names from the
design. `task-profiles.mjs` exports their language, initial file state, save-dialog
and overwrite-confirmation variants alongside the 100 fixture requests. A real
run still needs a verified environment and release freeze; diagnostic exports
with placeholder release metadata cannot supply that freeze.
This module neither supplies navigation workflows nor proves that tasks ran.

`fixture-reset.mjs` selects one fixture from the frozen 120-record export after
checking identities, assignments and hashes. It separates the app's startup
reset from the model's goal. Build the owned application with
`bash scripts/build-macos-mvp-fixture.sh`, then use the startup baseline:

```sh
node --import tsx scripts/mvp/launch-fixture.mts \
  --request-file /absolute/freeze/task-requests.json \
  --freeze-file /absolute/freeze/task-profile-freeze.json \
  --task-id SET-02 --seed 1 --out-dir /absolute/new-baseline
```

The baseline starts and stops only its owned app. Its private initial snapshot
is for offline evaluation, and it makes zero business runs or model calls.
Frozen-file validation alone does not verify the current environment or runtime
assets. Current state for any future model run must come from Source.

Run the MVP script regressions with:

```sh
node --import tsx --test scripts/mvp/*.test.mjs scripts/mvp/*.test.mts
```

The continuity tests use the already-built `target/debug/compile_device_context`
and the current Source contract. The corpus tests import workspace TypeScript,
so this combined command needs the `tsx` loader.

## Suite coordinator and receipt ports

`run-business-suite.mjs` validates the complete frozen 120-record identity set
before creating a new private output directory. `m5-full` selects all 100 fixture
runs and 20 TextEdit runs; `m3-fixture-diagnostic` selects all 20 fixture IDs at
one seed and cannot pass the business gate. Failures, unsupported tasks, missing
receipts and unrun identities remain in the original denominators.

The coordinator starts separately supplied trusted driver and evaluator programs:

```sh
node scripts/mvp/run-business-suite.mjs \
  --request-file /absolute/freeze/task-requests.json \
  --freeze-file /absolute/freeze/task-profile-freeze.json \
  --truth-file /absolute/freeze/evaluator-truth.json \
  --driver /absolute/path/to/node \
  --driver-arg /absolute/path/to/product-driver.mjs \
  --driver-command-manifest /absolute/path/to/driver-command.json \
  --evaluator /absolute/path/to/node \
  --evaluator-arg /absolute/path/to/independent-evaluator.mjs \
  --evaluator-command-manifest /absolute/path/to/evaluator-command.json \
  --out-dir /absolute/new-suite-output --scope m5-full
```

Each private command manifest binds the executable SHA-256, exact launch argv
prefix, and declared `entry`/`script`/`import` artifact hashes. The driver manifest
also matches `frozen_product_decision_program_sha256` to the environment freeze;
the evaluator uses `null` for that binding. The interpreter entry must be the
first argument, so Node startup options cannot run code before the hashed entry.
The hashes are checked before and after execution. The artifact set is a trusted
declaration; its transitive completeness is not independently verified.

The driver receives a goal containing only `objective` and `allowed_scope`, plus
a separate launcher reset file. The evaluator receives truth only after the
driver phase has exited, each coordinator-created process group is independently
absent, the same private temporary directory is empty, and every driver cleanup
report is consistent. Missing, replaced or nonempty temporary scopes, surviving
groups and unknown probes block the next task and truth staging. These probes
send signal zero only; timeout termination still targets the exact direct child.
Direct driver args, the reduced environment and model goal exclude truth. This
is a data-routing boundary; it does not enforce same-UID filesystem isolation.
Both programs must preserve design §14.7's independent truth boundary.

`business-run-receipt.mjs` validates typed decision/attempt/effect references,
hashed evidence files, final Source read metadata, and Host remote-call rows
scoped by Root, request/decision and generation. It retains nullable response
IDs, actual network counts, usage and cost rather than inventing them from call
admission. Promoted local S1 revisions have separate evidence; a local decision
with a teacher call cannot establish zero teacher calls. The receipt's normalized
`s2` source and Host plan mapping still require an actual driver audit.

The product business driver and full product auditor are not implemented by
this coordinator. `coordinator_scope_cleanup_probe_complete` covers the inherited
process group and the coordinator's temporary subtree. A child can leave that
group, and TextEdit can be launched by macOS outside it.
`product_owned_resource_coverage_verified=false` therefore keeps the evidence
and M5 business gates closed. Synthetic child programs in regression tests do
not execute business tasks. A real environment freeze, live Source/Root evidence
and complete product resource ownership are still required before a 100+20 run.

## Scoped operator goals

The normal product accepts frozen goals through the published preload/Main/Host
path and shows the Host-frozen scope in Goal confirmation. The objective remains
verbatim user text. Control scopes name one application and one registered
on/off control, with an optional exact group. Form scopes name one primary
control and one exact form context. Both prohibit unrelated controls. Main and
Host validate the complete selected scope; Host resolves the application to one
trusted pack and persists that binding without allowing planner changes. Queued
tasks retain the complete request.

Simple control scopes support Background AX and the fixed headless browser
profile with fresh current-UI readback. Foreground native scopes can also use
the exact Host-frozen navigation pair for UI revisit. Form scopes retain a
single primary field and its exact form context. Other navigation, ambiguous
controls, modal actions, scrolling, unregistered menu/row operations, file paths
and Computer coordinate proposals remain outside these scoped plans. Normal free-text tasks
retain their existing lifecycle and user confirmation.

The registered Reminder cadence popup uses one `ui.select_by_label` followed by
the frozen revisit. Its exact localized choices come from the trusted pack:
Daily/Weekly/Monthly or 每天/每周/每月. Both plan admission and runtime checks
require the selected label to equal the frozen desired value. The existing
menu transaction opens the current popup with `ShowMenu`, presses one anchored
menu item, and reads the actual value after the menu closes. Only then can the
Host-authorized revisit proceed; unknown input is never replayed.

For the registered manual-save control, the Host freezes one related Save
action and a visible receipt locator. Goal confirmation shows that action and
its group; its Commit permission covers only this exact Save target. The
original one-Goal confirmation model is preserved. The automatic-save control
has no Save action: the Host performs bounded Source-only polling before the
frozen revisit. Both paths require a unique visible receipt from a fresh Source
read whose positive revision advances beyond the pre-input receipt. Reset and
evaluator state never supply that online revision. Unknown Save input is not
replayed.

This operator constraint remains separate from frozen profile provenance.
The product driver structurally validates the frozen form family and checks the
exact primary/form scope shown in Goal confirmation against the selected frozen
request. That check does not establish that a form field is registered by the
trusted app pack: only FORM-01, FORM-02, FORM-03 and FORM-05 are currently authorized by
the fixture pack; other FORM identities remain denominator-preserving Host
refusals. The complete 100+20 path still needs the remaining registered fixture
operations, a real model connection and independent business verification.
Limited reference or catalog diagnostics do not enter the business denominator
or open the M5 gate. Any new post-Execute confirmation stops the driver for user
attention; it does not automatically approve another action.

## Main-owned frozen goals

A new freeze writes `task-goals.json` and `goal-catalog-manifest.json` alongside
the original three artifacts. The original request/freeze/truth hashes and format
remain unchanged. The goal catalog preserves all 100 fixture and 20 TextEdit
identities in the frozen order; it contains only identity and the projected
`{objective, allowed_scope}`. Reset state stays in the coordinator's request
export, and truth stays with the independent evaluator. The catalog manifest
binds both source exports, goal bytes, complete identity order and each opaque
selector. Goal catalog creation does not read the evaluator file.

Configure Main at startup with these four explicit values from the freeze CLI's
`goal_catalog_hashes`/`catalog_id` result:

```sh
CU_FROZEN_TASK_GOALS_PATH=/absolute/private-catalog/task-goals.json
CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH=/absolute/private-catalog/goal-catalog-manifest.json
CU_FROZEN_TASK_GOALS_SHA256=<task-goals.json raw SHA-256>
CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256=<goal-catalog-manifest.json raw SHA-256>
```

Both files must be stable, canonical, current-user regular files with one link
and mode 0600, in the same canonical directory with mode 0700. Main reads only
these two files. It never reads driver reset, environment or evaluator exports.
No configuration retains the ordinary free-text and single-control UI; partial
or invalid configuration shows an unavailable frozen catalog. A configured
catalog shows all 120 entries. The public `frozenTaskCatalog()` returns display
metadata only, and `createFrozenTask({catalog_id, selector})` resolves the exact
request in Main; it accepts no caller-supplied objective, scope, path or reset.

`scope_compatible` means that the entire request scope can be expressed by the
current single-control operator contract. A form record must retain both its
primary control and form context. Compatibility is not application registration,
execution-domain support, model readiness or business acceptance. LIST, MODAL
and TextEdit requests remain explicit unsupported entries. FORM and SET requests
still face Host trusted-pack, domain and Goal checks; only registered targets
are admitted. No scope is narrowed to make a frozen request executable.

Host schema 7 stores the admitted profile's catalog/selector/source hashes and
identity in immutable `root_tasks.frozen_profile_json`. Queueing and recovery
retain it; state updates cannot replace it. It grants no action capability and
is excluded from planning context. The ordinary scope, Goal confirmation,
Reserve/Activate and execution gates remain responsible for authority.

## Independent Host facts and TextEdit file condition

`host-ledger-evidence.mjs` reads one exact Root from a normal Host database in a
read-only SQLite transaction against current Host schema 7. Earlier schemas
are rejected. It exports raw decision sources and
decision/attempt/map/outbox references, plus separate S2/Teacher and Computer
call metadata and the selected Root’s bounded frozen admission identities.
It does not read task text, Goal/Plan payloads, effect JSON,
outbox payloads, provider request/response bodies, or event details. Missing
S2-to-plan links, local model revisions and actual network request counts remain
explicitly unknown; admission rows cannot invent those facts.

```sh
node scripts/mvp/host-ledger-evidence.mjs \
  --db /absolute/private-supervisor/host.db --root root-id \
  --output /absolute/private-evidence/new-root-facts.json
```

The caller must pass canonical paths and capture facts before the Host's
temporary directory is removed. The database must have one link, belong to the
current user, and allow no group/other write or execute access. Its immediate
parent must be private mode 0700. Export files are new mode-0600 files and are
never overwritten. Field and total row/byte limits reject incomplete exports.
This tool needs Node with `DatabaseSync.enableDefensive()` (24.12+, 25.1+, or a
later supporting release); see the [Node SQLite API](https://nodejs.org/download/release/latest-v24.x/docs/api/sqlite.html#databaseenabledefensiveactive).
The tested system runtime is Node 25.2.1; Electron's runtime is separate.

`textedit-file-oracle.mjs` independently reads only the exact named output file
inside the frozen private directory, checks a stable single-link regular file,
and compares its plain UTF-8 bytes to the frozen hash and length. It neither
writes files nor exposes contents to online components. A matching file condition
does not prove UI close/reopen, actual overwrite confirmation, scope side
effects or business completion; those states remain unchecked/unknown until
the product trace and independent UI auditor establish them.

Aggregate a separately produced report with:

```sh
node scripts/mvp/report-business-results.mjs /absolute/path/report.json
```

The input is `{ "version": 1, "suite": "fixture" | "textedit", "runs": [...] }`.
Each run identifies `task_id` and integer `seed` (`1` for TextEdit). Admitted runs
record `accepted=true`, status (`completed`, `failed`, `timeout`, `unknown`,
`unverified`, or `cancelled`), `system_reported_completed`, independent verdict
(`satisfied`, `not_satisfied`, or `unknown`), `cold_start`, and
`knowledge_has_task_workflows`. Rejected runs record `accepted=false`,
`system_reported_completed=false`, and a nonempty `rejection_reason`.

An unknown receipt remains admitted even when a later value matches. Missing or
rejected cases cannot shrink the frozen quality gate. A completion whose
independent verdict is unsatisfied counts as a false completion. An unknown
verdict is a separate unverified completion; both block the quality gate.
Fixture quality additionally requires at least four completions for every task;
both suites require 95% completion and zero false completions under cold-start
conditions without task workflows.

The output always has `aggregation_only=true`, `evidence_validity_audited=false`,
and `mvp_complete=false`. Input assertions are reporting facts, not independent
verification. Authoritative trace/UI/evaluator evidence, manifests, fault,
model, environment, timing, and signed-package checks still require the full
§26/§28 completion audit. A threshold result from synthetic test rows cannot
substitute for those checks.

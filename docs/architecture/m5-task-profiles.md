# M5 frozen task profiles and export boundary

This layer prepares a reviewable business-evaluation plan. It does not launch an app, create a task, contact a model, collect a result, or claim that a business case passed. `business-results.mjs` remains the authority for report IDs and denominators; this profile module supplies the preassigned task conditions that were missing from those report slots.

## Frozen denominator and assignments

The fixture suite is exactly the 20 IDs in `business-results.mjs`, each with seeds 1 through 5: 100 records. TextEdit is exactly `TEXTEDIT-01` through `TEXTEDIT-20`, with seed 1: 20 records. The request export contains 120 records. A missing or invalid manifest prevents all three freeze files from being written.

For each fixture task, let `i` be its zero-based position in the canonical 20-ID list and `s = seed - 1`. The variant is assigned before execution:

- Language is `en-US` when `(i + s) % 2 == 0`, otherwise `zh-CN`.
- Theme is `light` when `(i + floor(s / 2)) % 2 == 0`, otherwise `dark`.
- Layout is `compact`, `standard`, `wide`, or `two_column` at index `(i + s) % 4`.

Across the fixture denominator this produces 50 runs in each language, 50 in each theme, and 25 in each layout. TextEdit alternates English and Chinese by task ID (`TEXTEDIT-01` is English); for zero-based ID index `j`, theme uses `(j + floor(j / 3)) % 2` and layout uses `(3j) % 4` with the same ordered values. `TEXTEDIT-01..10` are new-file cases; `TEXTEDIT-11..20` are existing-file cases. No variant changes after results are observed.

The 20 semantic conditions are:

| IDs | Frozen condition |
|---|---|
| SET-01..06 | Leave an already-off setting unchanged; turn an on setting off; explicitly save an on change; wait for autosave; revisit and read back; disambiguate identical labels by group while preserving the other control. |
| FORM-01..07 | Set a single-line field; set exact two-line text; choose a dropdown value; choose a radio value; set a checkbox to its assigned boolean; correct an invalid value until validation passes; set a field that starts below the viewport. |
| LIST-01..04 | Locate by label; distinguish two same-named rows by owner and status; select a virtualized row outside the initially rendered set; keep record identity after an update changes sort order. |
| MODAL-01..03 | Dismiss the known informational blocker without changing the background; operate on a control inside a modal; complete a modal choice while preserving and not operating background controls. |

Task requests state the user objective, permitted semantic scope, and initial UI state. They contain no selector, coordinate, step list, navigation path, or evaluator field. User-requested target values and document text are included because they are the task itself; they are not hidden evaluator truth.

## TextEdit cases

Each TextEdit request names the absolute task output directory, one deterministic unique filename (`cu-mvp-textedit-01.txt` through `cu-mvp-textedit-20.txt`), plain-text format, and exact requested content. The ten new-file cases start with the target absent and require the first-save dialog. The ten existing-file cases start with a disposable test document and require a destructive overwrite confirmation before replacement.

Existing-file evaluator records preassign a unique confirmation ID and the driver answer `approve`, require exactly one confirmation event, name the fields the future run must record, and state that the event has not happened yet. This is a planned response, not a completed confirmation. The profile tool never creates or replaces those files. The eventual runner must isolate the output directory, record the actual confirmation event, save, close, reopen in TextEdit, and read the result back through the UI. The independent evaluator then checks the saved artifact without supplying its expected digest to the online model.

## Export separation

The freeze writes three exclusive files:

- `task-requests.json` is the driver export. Each record contains `suite`, `task_id`, `seed`, and `task_request` with exactly `objective`, `allowed_scope`, and `initial_state`. The initial state describes fixture reset/setup conditions and is not model evidence.
- `projectTaskRequestForModel` and `projectTaskRequestsForModel` produce the Goal/model input from that driver export. Each projected record contains only `objective` and `allowed_scope`. The user's requested values and document body remain part of the objective; reset state and evaluator-only fields are omitted.
- `evaluator-truth.json` is a separate offline-only file with expected final predicates, saved-content digests, forbidden side effects, and the destructive-confirmation contract. It is not embedded in or hashed into the online request file.
- `task-profile-freeze.json` records the sanitized fixed environment, denominators, and separate SHA-256 hashes of the request and evaluator files. Its stable content hash excludes only its own hash field.

At run time, the model's current state must come from a real Source observation. The runner must not turn a manifest `initial_state` into an `EnvironmentSnapshot` or present that reset recipe as a live observation.

No timestamp, random ID, environment-asset source path, execution receipt, business score, or success claim is added to the frozen files. The TextEdit output directory remains in the task request because it is part of the user's requested scope. Stable key ordering and a final newline make identical inputs produce identical bytes. File creation uses exclusive `wx`; choose a new output directory for another freeze. Existing frozen files are never replaced.

## Environment manifest

The local JSON manifest must provide all of these fields:

```json
{
  "version": 1,
  "os": { "name": "macOS", "build": "<exact build>" },
  "display": { "width_px": 2560, "height_px": 1600, "dpi_x": 220, "dpi_y": 220 },
  "power_mode": "AC balanced",
  "hardware": { "model": "Apple M2 Max", "architecture": "arm64", "memory_gib": 64 },
  "app": { "name": "Interaction Fixture", "bundle_id": "com.lingxi.computer-user.mvp-fixture", "version": "1.0.0" },
  "textedit_app": { "name": "TextEdit", "bundle_id": "com.apple.TextEdit", "version": "<exact installed version>" },
  "model": { "name": "<exact model>", "version": "<exact release>" },
  "graph": { "release": "<exact graph release>" },
  "preprocess": { "release": "<exact preprocessing release>" },
  "program": { "release": "<exact program release>" },
  "knowledge": { "release": "<fixed release>", "task_workflows_included": false },
  "asset_root": "/absolute/local/read-only-root",
  "artifact_assets": {
    "app": { "relative_path": "fixture-executable", "max_bytes": 100000000, "sha256": "<64 lowercase hex digits>" },
    "textedit_app": { "relative_path": "textedit-executable", "max_bytes": 100000000, "sha256": "<64 lowercase hex digits>" },
    "model": { "relative_path": "model.bin", "max_bytes": 1000000000, "sha256": "<64 lowercase hex digits>" },
    "graph": { "relative_path": "graph.bin", "max_bytes": 100000000, "sha256": "<64 lowercase hex digits>" },
    "preprocess": { "relative_path": "preprocess.bin", "max_bytes": 10000000, "sha256": "<64 lowercase hex digits>" },
    "program": { "relative_path": "program.bin", "max_bytes": 10000000, "sha256": "<64 lowercase hex digits>" },
    "knowledge": { "relative_path": "knowledge.bin", "max_bytes": 100000000, "sha256": "<64 lowercase hex digits>" }
  }
}
```

The `textedit_app` metadata records TextEdit's exact installed version and required `com.apple.TextEdit` bundle ID; the separate `artifact_assets.textedit_app` entry pins its executable artifact by hash. The fixture app, TextEdit app, model, graph, preprocessing, program, and knowledge artifact classes are required; additional named artifacts can be included and are also hashed. Each asset is a single regular, non-symlink file under `asset_root`, has an explicit positive `max_bytes` bound, and must match its declared lowercase SHA-256 digest. Each bound is at most 16 GiB and the declared total across the artifact set is at most 64 GiB. The module reads and hashes the bounded file descriptor, checks file identity before and after the read, and emits only artifact hashes. Local asset paths are not copied to the freeze manifest. Knowledge releases containing task-specific workflows are rejected for the cold-start plan.

## CLI

`scripts/mvp/fixture-reset.mjs` validates both frozen files, their content and
environment hashes, all 120 identities, the assigned fixture variants, and the
registered Interaction Fixture identity before selecting a reset. It gives the
app only `{version, task_id, seed, initial_state}` and gives the model only
`{objective, allowed_scope}`. Neither projection includes evaluator truth.
This validation does not re-observe the environment or verify the runtime asset
bytes, and therefore cannot establish a formal business-run freeze by itself.

Build the owned native fixture with `bash scripts/build-macos-mvp-fixture.sh`.
The app has a separate private snapshot for offline evaluation; that snapshot
cannot replace a current Source observation. Startup baselines and synthetic
projection regressions do not count toward the 100 business runs.

Run from the repository root after preparing and reviewing the local environment manifest and an isolated absolute task output directory:

```sh
node scripts/mvp/freeze-task-profiles.mjs \
  --environment /absolute/path/environment.json \
  --task-output-dir /absolute/path/isolated-task-output \
  --out-dir /absolute/path/new-profile-freeze
```

The command prints `profile_plan_frozen`, `execution_started: false`, and `business_results: not_run` with denominators and content hashes. It performs no app, model, network, GUI, or evaluator operation. The included `task-profiles.test.mjs` covers denominators, per-ID and variant assignments, semantic conditions, TextEdit overwrite-event planning, projection of all 120 requests without reset/evaluator data, request/truth separation, hash stability, required environment fields, bounded assets, and exclusive output behavior.

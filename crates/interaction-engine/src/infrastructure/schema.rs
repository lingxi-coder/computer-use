//! The SQLite schema. Every table has exactly one owning module, noted above
//! its definition; only that module's store writes to it.

/// Applied in order; `PRAGMA user_version` records how many have run.
pub const MIGRATIONS: &[&str] = &[
    r#"
-- Owner: infrastructure
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE event_log (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms   INTEGER NOT NULL,
    root_id TEXT,
    kind    TEXT NOT NULL,
    detail  TEXT NOT NULL
);

-- Owner: TaskRuntime
CREATE TABLE root_tasks (
    root_id          TEXT PRIMARY KEY,
    seq              INTEGER NOT NULL,
    user_text        TEXT NOT NULL,
    state            TEXT NOT NULL,
    state_origin     TEXT,
    task_revision    INTEGER NOT NULL,
    root_epoch       INTEGER NOT NULL,
    draft_json       TEXT,
    goal_json        TEXT,
    plan_revision    INTEGER NOT NULL DEFAULT 0,
    plan_json        TEXT,
    created_ms       INTEGER NOT NULL,
    frozen_ms        INTEGER,
    deadline_ms      INTEGER,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    cleanup          TEXT NOT NULL DEFAULT 'not_needed',
    activation_seq   INTEGER NOT NULL DEFAULT 0,
    failure          TEXT
);
CREATE TABLE budget_ledger (
    root_id           TEXT PRIMARY KEY REFERENCES root_tasks(root_id),
    attempts_used     INTEGER NOT NULL DEFAULT 0,
    observations_used INTEGER NOT NULL DEFAULT 0,
    recoveries_used   INTEGER NOT NULL DEFAULT 0,
    replans_used      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE attempts (
    attempt_id         TEXT PRIMARY KEY,
    root_id            TEXT NOT NULL REFERENCES root_tasks(root_id),
    decision_id        TEXT NOT NULL,
    generation         INTEGER NOT NULL,
    state              TEXT NOT NULL,
    skill              TEXT NOT NULL,
    subgoal            TEXT NOT NULL,
    quota_actions      INTEGER NOT NULL,
    quota_observations INTEGER NOT NULL,
    activation_seq     INTEGER NOT NULL,
    root_epoch         INTEGER NOT NULL,
    task_revision      INTEGER NOT NULL,
    plan_revision      INTEGER NOT NULL,
    deadline_ms        INTEGER NOT NULL,
    effect_class       TEXT NOT NULL,
    source             TEXT NOT NULL,
    refunded           INTEGER NOT NULL DEFAULT 0,
    created_ms         INTEGER NOT NULL,
    intent_id          TEXT
);
CREATE INDEX attempts_by_root ON attempts(root_id, state);

-- Owner: Reaction
CREATE TABLE decisions (
    decision_id      TEXT PRIMARY KEY,
    root_id          TEXT NOT NULL,
    generation       INTEGER NOT NULL,
    state            TEXT NOT NULL,
    source           TEXT,
    created_ms       INTEGER NOT NULL,
    consumed_attempt TEXT
);
CREATE TABLE attempt_map (
    decision_id TEXT PRIMARY KEY,
    generation  INTEGER NOT NULL,
    attempt_id  TEXT NOT NULL UNIQUE
);
CREATE TABLE admission_outbox (
    attempt_id TEXT PRIMARY KEY,
    payload    TEXT NOT NULL,
    delivered  INTEGER NOT NULL DEFAULT 0
);

-- Owner: Verification
CREATE TABLE effect_results (
    result_id TEXT NOT NULL,
    revision  INTEGER NOT NULL,
    root_id   TEXT NOT NULL,
    json      TEXT NOT NULL,
    PRIMARY KEY (result_id, revision)
);

-- Owner: Memory
CREATE TABLE releases (
    release_id    TEXT PRIMARY KEY,
    published_seq INTEGER NOT NULL,
    manifest      TEXT NOT NULL
);
CREATE TABLE release_members (
    release_id  TEXT NOT NULL REFERENCES releases(release_id),
    revision_id TEXT NOT NULL,
    PRIMARY KEY (release_id, revision_id)
);
CREATE TABLE root_release_pins (
    root_id    TEXT PRIMARY KEY,
    release_id TEXT NOT NULL
);
CREATE TABLE quarantine (
    revision_id TEXT PRIMARY KEY,
    reason      TEXT NOT NULL,
    seq         INTEGER NOT NULL
);
"#,
    r#"
-- Owner: Deliberation. One row per admitted remote call, including cancelled
-- and failed turns. An unfinished row after a crash has unknown final usage.
CREATE TABLE model_calls (
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    request_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    finished_ms INTEGER,
    outcome TEXT,
    usage_json TEXT NOT NULL,
    PRIMARY KEY(root_id, request_id, generation)
);
"#,
    r#"
-- Owner: Deliberation. S1 decision calls are distinct from S2 planning calls.
-- An unfinished row after a crash keeps usage unknown; payloads are never stored.
CREATE TABLE decision_model_calls (
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    decision_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    finished_ms INTEGER,
    outcome TEXT,
    usage_json TEXT NOT NULL,
    PRIMARY KEY(root_id, decision_id, generation)
);
"#,
    r#"
-- Owner: Engine owned-document revisit. These records bind a successful Save
-- receipt to one Host-authorized reopen transition without storing document text.
CREATE TABLE owned_save_evidence (
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(attempt_id),
    task_revision INTEGER NOT NULL,
    root_epoch INTEGER NOT NULL,
    registration_id TEXT NOT NULL,
    intent_id TEXT NOT NULL,
    span_from_ms INTEGER NOT NULL,
    span_to_ms INTEGER NOT NULL,
    expected_sha256 TEXT NOT NULL,
    expected_byte_length INTEGER NOT NULL,
    device_session_epoch INTEGER NOT NULL,
    source_namespace TEXT NOT NULL,
    clock_domain TEXT NOT NULL,
    completed_ms INTEGER NOT NULL,
    json TEXT NOT NULL
);
CREATE INDEX owned_save_by_root ON owned_save_evidence(root_id, completed_ms);
CREATE TABLE owned_revisit_transitions (
    transition_id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    save_attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(attempt_id),
    task_revision INTEGER NOT NULL,
    root_epoch INTEGER NOT NULL,
    state TEXT NOT NULL,
    challenge_json TEXT NOT NULL,
    completion_json TEXT,
    created_ms INTEGER NOT NULL,
    deadline_ms INTEGER NOT NULL,
    failure TEXT
);
"#,
    r#"
-- Owner: Deliberation computer accounting. These independent high-water marks
-- are committed before IDs are returned and are never reconstructed from rows.
CREATE TABLE computer_accounting_counters (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_generation INTEGER NOT NULL CHECK (next_generation BETWEEN 0 AND 9007199254740991),
    next_batch_id INTEGER NOT NULL CHECK (next_batch_id BETWEEN 0 AND 9007199254740991)
);
INSERT INTO computer_accounting_counters(singleton, next_generation, next_batch_id)
VALUES (1, 0, 0);

-- Owner: Deliberation. Computer-use model calls have independent rows from
-- S1 decision calls and S2 planner calls. Payloads are never persisted.
CREATE TABLE computer_model_calls (
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    request_id TEXT NOT NULL,
    generation INTEGER NOT NULL UNIQUE CHECK (generation BETWEEN 1 AND 9007199254740991),
    task_revision INTEGER NOT NULL CHECK (task_revision BETWEEN 1 AND 9007199254740991),
    root_epoch INTEGER NOT NULL CHECK (root_epoch BETWEEN 1 AND 9007199254740991),
    snapshot_id TEXT NOT NULL,
    structure_sequence INTEGER NOT NULL CHECK (structure_sequence BETWEEN 1 AND 9007199254740991),
    deadline_ms INTEGER NOT NULL CHECK (deadline_ms BETWEEN 1 AND 9007199254740991),
    plan_revision INTEGER NOT NULL CHECK (plan_revision BETWEEN 1 AND 9007199254740991),
    subgoal_id TEXT NOT NULL,
    created_ms INTEGER NOT NULL CHECK (created_ms >= 0),
    stage TEXT NOT NULL CHECK (stage IN ('begin', 'initial_screenshot', 'fresh_screenshot')),
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    started_ms INTEGER NOT NULL CHECK (started_ms >= 0),
    finished_ms INTEGER,
    outcome TEXT,
    usage_json TEXT NOT NULL,
    PRIMARY KEY(root_id, request_id, generation)
);
CREATE INDEX computer_model_calls_by_root_started
    ON computer_model_calls(root_id, started_ms, generation);
"#,
    r#"
-- Owner: TaskRuntime. The user-authorized control boundary and its trusted
-- application binding are immutable for the lifetime of a root.
ALTER TABLE root_tasks ADD COLUMN allowed_scope_json TEXT;
ALTER TABLE root_tasks ADD COLUMN allowed_scope_app TEXT;
"#,
    r#"
-- Owner: TaskRuntime. Catalog provenance is fixed at admission, separate from
-- GoalDraft, planning context, effect evidence, and the permission boundary.
ALTER TABLE root_tasks ADD COLUMN frozen_profile_json TEXT;
"#,
    r#"
-- Owner: Deliberation and TaskRuntime. A validated planner request is retained
-- with the plan revision it produced; legacy requests keep a null reason.
ALTER TABLE model_calls ADD COLUMN reason TEXT;
ALTER TABLE decisions ADD COLUMN plan_revision INTEGER;
CREATE TABLE plan_installations (
    root_id TEXT NOT NULL REFERENCES root_tasks(root_id),
    plan_revision INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('proposed', 'installed')),
    attribution_status TEXT NOT NULL CHECK (attribution_status IN ('model_request', 'unknown')),
    request_id TEXT,
    generation INTEGER,
    reason TEXT,
    installed_ms INTEGER,
    PRIMARY KEY(root_id, plan_revision),
    UNIQUE(root_id, request_id, generation),
    CHECK (
        (attribution_status = 'model_request'
            AND request_id IS NOT NULL AND generation IS NOT NULL AND reason IS NOT NULL)
        OR
        (attribution_status = 'unknown'
            AND request_id IS NULL AND generation IS NULL AND reason IS NULL)
    ),
    CHECK (
        (state = 'proposed' AND installed_ms IS NULL)
        OR (state = 'installed' AND installed_ms IS NOT NULL)
    ),
    FOREIGN KEY(root_id, request_id, generation)
        REFERENCES model_calls(root_id, request_id, generation)
);
CREATE INDEX plan_installations_by_root_state
    ON plan_installations(root_id, state, plan_revision);
"#,
];

//! The SQLite schema. Every table has exactly one owning module, noted above
//! its definition; only that module's store writes to it.

/// Applied in order; `PRAGMA user_version` records how many have run.
pub const MIGRATIONS: &[&str] = &[r#"
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
"#];
